# Plugin development runner implementation plan

Status: implemented and locally verified; not published to npm.
Branch: `feat/plugin-dev-runner`
Base: `origin/main` at `e006bb00e45ae34763600c6b28f49a1b6bd47efb`.

## Outcome

A developer runs an existing compiled plugin, opens its UI, and sends realistic
Temps events using `bunx @temps-sdk/cli`, without a Temps server, database,
Docker, login, or AI-provider credentials. Support TypeScript source execution
as well. Plugins run as the developer's OS account; this is not a sandbox.

## Proposed commands

```bash
# Foreground runner; argument is an executable, not a project directory.
bunx @temps-sdk/cli plugin dev ./my-plugin --session crawl --grant events_read

# Source execution: executable and argument vector, never shell evaluation.
bunx @temps-sdk/cli plugin dev --session crawl --exec bun -- run src/index.ts

# Another terminal: generate a valid event with coherent numeric fixture IDs.
bunx @temps-sdk/cli plugin dev emit deployment.succeeded --session crawl \
  --project-id 1 --environment-id 1 --environment production \
  --deployment-id 42 --url https://example.com

bunx @temps-sdk/cli plugin dev events
bunx @temps-sdk/cli plugin dev emit --session crawl --file deployment.json
bunx @temps-sdk/cli plugin dev emit --session crawl --file deployment.json --repeat 2
bunx @temps-sdk/cli plugin dev status --session crawl --json
bunx @temps-sdk/cli plugin dev grants set --session crawl --grant events_read
bunx @temps-sdk/cli plugin dev grants set --session crawl --clear
```

`events` lists supported fixtures and required fields. `--repeat` delivers the
same event ID; `--count` generates distinct IDs for burst testing. Both are
bounded and mutually exclusive. Export the generated envelope with `--json`.
Grant replacement semantics match the existing grant CLI: `set` replaces the
full set; `--clear` removes all. No grants by default. Explain denied delivery
with the exact grant command required; never silently grant declared access.

Flags: `--session`, `--port` (loopback, default OS-assigned), `--data-dir`,
`--fixtures`, `--grant`, and startup timeout. `--exec <executable>` consumes
arguments after `--`; reject combining it with a binary argument. Normal
shutdown is Ctrl-C; explicit session selection avoids sending to another runner.

## Verified integration points

- `apps/temps-cli/src/commands/plugin/index.ts`: existing Commander group and
  Bun guard; register dev here. Test positional arguments versus nested commands.
- `apps/temps-cli/src/cli.ts`: global hooks; local dev must not invoke API login,
  read provider credentials, or contact an active remote context.
- `apps/temps-cli/package.json`: package currently builds for Node and rewrites
  its shebang. Plain `bunx` must be tested from a packed artifact. If necessary,
  re-exec only the dev command under Bun with preserved argv/stdio/exit status;
  do not make all existing CLI commands require Bun. Include an actionable
  `bunx --bun` diagnostic when Bun cannot be located. Prevent re-exec loops.
- `sdks/node/packages/plugin-sdk/src/{runtime,launch,protocol,client,types}.ts`:
  protocol-2 hello, private stdin launch configuration, ready, Unix socket,
  authenticated channel, and nested `call` / `outcome` frames.
- `crates/temps-core/src/external_plugin/{channel,manifest,actor}.rs` and
  `crates/temps-plugin-sdk/src/{protocol,runtime,auth}.rs`: authoritative Rust
  protocol and permissions. Use both SDKs as conformance clients.
- `crates/temps-external-plugins/src/{event_listener,proxy,host_api}.rs`:
  event payload mapping, subscription matching, grant checks, proxy semantics.
  Host delivery prefers WebSocket and falls back to authenticated `POST /_events`.
- Existing `crates/temps-cli/tests/plugin_sdk_conformance.rs` validates API
  endpoint coverage; it does not validate the runner's wire protocol.

## Architecture and scope

Keep the implementation inside `apps/temps-cli/src/commands/plugin/dev/`.
Command handlers parse input and render output; runner modules own lifecycle,
transport, fixtures, permission decisions, and session state. Proposed modules:
`index.ts`, `runner.ts`, `protocol.ts`, `proxy.ts`, `session.ts`, `events.ts`,
`fixtures.ts`, `host.ts`, `errors.ts`, plus colocated tests and subprocess fixtures.
Split only where responsibilities justify it; no separate published package yet.

One foreground process owns one plugin, its channel, preview proxy, and private
control socket. Secondary commands use the control socket, not the preview HTTP
server. Store runtime metadata and plugin data outside the repository in a
private per-user directory, keyed by validated session name. Keep Unix socket
paths short. Lock sessions to reject concurrent reuse. On restart preserve plugin
data and grants, rotate ephemeral authentication, and verify stale metadata by
connecting before cleanup; never kill an arbitrary PID from a file.

Support macOS/Linux first because the current plugin transport is Unix sockets.
On unsupported systems fail early with a clear platform explanation. Do not
claim cross-platform support based only on the CLI's existing build targets.

### Process and protocol

Spawn with an argument array and `--socket-path` / `--data-dir`. Read bounded
newline-delimited hello/ready messages, validate protocol and manifest, and send
authentication over private stdin. Set database URL and host data directory to
null; identify unsupported legacy privileged plugins explicitly. Drain stderr
through bounded logging without interfering with handshake stdout. Connect the
channel promptly: SDK onStart can depend on it. Ready is transport readiness,
not proof that asynchronous plugin initialization has completed.

Correlate concurrent calls; validate incoming JSON, envelopes, sizes, and IDs.
Return actual protocol errors for unknown methods and missing fixtures. Bound
handshake duration, pending work, body sizes, and log retention. Suggested runner
limits: 64 KiB handshake line, 1 MiB event/control payload, 100 queued events,
8 concurrent deliveries, 1,000-event burst maximum; validate against host limits
before fixing defaults and document differences. Stream preview responses.

Shutdown stops intake, closes connections, signals only owned subprocesses,
waits a bounded grace period, and cleans only its own temporary sockets. Preserve
reports/data. Test early exit, hangs, occupied ports, and interrupted startup.

### Preview and local access

Serve a loopback-only preview URL and proxy using the host's plugin URL/base-path
conventions. Verify relative assets, SPA routes, queries, redirects, and cookies.
Replace caller-supplied Temps identity/auth headers with a synthetic local user;
never forward privileged headers directly. Support selecting synthetic admin or
reader roles, separate from plugin host grants. Block browser access to internal
channel/event/control endpoints. Enforce Host/Origin checks against cross-site
requests and DNS rebinding, protect state-changing preview requests with a local
session mechanism, and keep control secrets out of URLs and logs. Show a clear
local simulation label and the exact preview URL in terminal output.

### Host fixtures and AI

Implement capability discovery, project/environment/deployment read fixtures,
and deterministic mock AI success/failure/delay/timeout scenarios. Use the real
permission enum, capability response shape, and limits. Re-evaluate permissions
on every call and event; grants changes update discovery immediately. Enforce
mock quota and concurrency admission atomically, including release on errors.
Persist quota counters if restart continuity is advertised; otherwise label
session-scoped counters explicitly. Keep synthetic actor attribution separate
from the synthetic browser user. Logs are simulator records, not production
audit persistence evidence.

Use a versioned, validated JSON fixtures file with internally consistent IDs.
Unsupported host APIs return explicit errors instead of fabricated success.
No remote API passthrough, actual AI calls, or database provisioning in v1.

### Event realism

Generate built-in fixtures from the actual host event mapping: start with all
currently mapped deployment, project, and domain events. Numeric project IDs and
payload shapes must match Rust; names alone are not IDs. Enforce exact/prefix/*
subscriptions and `events_read`, including changes while running. Validate
custom envelopes; clearly label custom types that the host does not emit.

Use WebSocket first; provide an explicit HTTP transport override for testing the
fallback endpoint without inventing a channel reconnection lifecycle. Test the
automatic fallback when the channel is genuinely unavailable. Distinguish
`sent`, `HTTP accepted`, `denied`, and `failed`: neither current transport proves
completion of the plugin's async handler. Never claim a crawl completed because
event transmission succeeded. The TypeScript SDK currently invokes onEvent
without awaiting it; any SDK error handling correction discovered here needs
its own regression coverage, without inventing a host acknowledgment protocol.

No automatic replay or retries in v1: expose explicit repeat/burst controls, and
log delivery IDs so developers can test their own idempotency and recovery.

## Implementation milestones

1. **Contract and packaging tests.** Capture golden JSON fixtures from Rust
   serialization and actual event mapping. Add Rust tests that verify committed
   fixtures, and Bun tests that consume the same fixtures. Cover both success
   and error envelopes, launch configuration, permissions, and events. Verify
   the packed CLI enters the Bun runner using the requested plain bunx command.
2. **Runner and preview.** Implement process lifecycle, handshake, authenticated
   channel, private session/control transport, UI proxy, and status. Run an
   actual SDK plugin subprocess, not just transport mocks. Demonstrate a compiled
   executable and TypeScript source path; test malformed startup and cleanup.
3. **Events.** Add fixture listing/emission, file validation, subscription/grant
   enforcement, live revocation, duplicate IDs, bounded bursts, and transport
   selection. Confirm event receipts in a plugin-owned test log.
4. **Host simulation.** Add read fixtures, discovery, mock AI scenarios, synthetic
   actor logs, and persisted grant editing. Test denials, missing configuration,
   quotas under concurrency, and cancellation without leaked slots.
5. **Dogfood, docs, release evidence.** Exercise Site Crawl plus minimal Rust and
   TypeScript fixtures. Update CLI README, generated command docs, and plugin-init
   guidance. Pack and run from an empty directory with no Temps login. Obtain
   security-auditor review before a signed-off commit/PR. No merge or package
   publication as part of implementation without the corresponding task scope.

## Acceptance and validation

- Bun unit and subprocess suites, CLI `bun run typecheck`, CLI build and packed
  artifact smoke test. SDK tests/build if touched. Include CLI help and argument
  parsing coverage so `emit` never becomes an executable filename accidentally.
- Rust fixture/conformance tests in affected crates; `cargo check --lib`, relevant
  `cargo test --lib -p ...`, and warning-free clippy if Rust is changed. Fixture
  generation occurs during development/CI, never on the user's machine.
- Run TypeScript and compiled Rust plugins with the same handshake/event corpus.
  Browser evidence proves assets load and API requests receive synthetic roles.
- Site Crawl: configure automation, grant events_read, emit production success,
  observe one completed saved report; repeat the same event and deployment IDs,
  confirm no duplicate; emit staging, confirm configured filtering; revoke grant,
  confirm no delivery; restart with the same data directory, confirm report and
  settings survive. Use a controlled crawl fixture compatible with its DNS
  protections; never weaken crawler protections to allow a localhost test site.
- Check failures: unknown event/fixture, unsubscribed event, denied permission,
  malformed handshake, missing binary, incompatible binary, child crash, timeout,
  occupied session, huge payload, queue saturation, spoofed headers, cross-site
  control attempts, and clean shutdown. Errors name the session/operation and
  explain a concrete next step without exposing secrets.
- Simulator tests prove the developer runner, not production installation or real
  provider integration. Record that distinction in the PR evidence.

## Deferred

Watch/rebuild automation, rich event inspector UI, scenario assertions language,
real-provider access, remote host passthrough, native Windows transport, plugin
marketplace installation, and OS sandboxing. Keep v1 useful with a foreground
runner, visible logs, repeatable events, and a working plugin preview.


## Implementation evidence (2026-09-17)

Implemented in `apps/temps-cli/src/commands/plugin/dev/`, with a compiled/source
TypeScript deployment-journal example and a Rust SDK conformance example.

Verified on macOS with Bun 1.4.2:

- `bun test` in `apps/temps-cli`: **1,184 passed**, zero failures, including
  30 development-runner tests. These run real SDK subprocesses, not just mocks.
- `bun run typecheck` and `bun run build`: passed.
- `bun run scripts/test-plugin-dev-package.ts`: passed. This packs the CLI,
  installs it in a separate temporary consumer directory, compiles the example,
  and invokes plain `bunx @temps-sdk/cli plugin dev <binary>`. It checks UI,
  duplicate events, mock AI, revocation, and SIGTERM cleanup.
- `cargo check --lib -p temps-plugin-sdk`: passed without warnings.
- `cargo test --lib -p temps-plugin-sdk`: **16 passed**.
- `cargo test -p temps-plugin-sdk --example plugin-dev-probe`: **2 passed**.
- `cargo clippy -p temps-plugin-sdk --lib --example plugin-dev-probe -- -D warnings`:
  passed. The actual Rust example also decoded the runner's capabilities and
  project responses, accepted WebSocket and HTTP events, and observed revocation.
- Browser dogfood with the existing compiled Site Crawl plugin: UI loaded under
  the runner; automation settings could be edited; a simulated production
  deployment started a real crawl of `https://example.com`. One page completed,
  with zero route errors and two SEO issues. Duplicate event delivery produced
  one report. A staging event produced no report with production-only enabled.
  Revoking Events read blocked the next event. Restart retained the report,
  automation settings, and revoked grant state. The other plugin worktree was
  read-only throughout; no plugin source or existing preview was changed.
- Independent security review signed off after fixes for cookie isolation and
  descendant-process cleanup. Both fixes have executable regression coverage;
  startup interruption, header spoofing, reserved paths, cross-site requests,
  source execution, and invalid handshakes are covered too.

The CLI's generated command references were refreshed to pass the repository's
synchronization tests. This also brought previously missing registered commands
into those generated references; no unrelated command implementation changed.

Implementation decisions relative to the proposal:

- Conformance uses the actual Rust SDK subprocess as well as the actual
  TypeScript SDK, rather than a second hand-maintained set of golden frames.
  Build the Rust example before running Bun tests to enable the cross-SDK test;
  CLI-only checkouts skip that one test when the binary is absent.
- Events have no waiting queue or implicit replay: emission is explicit and
  bounded, with at most eight simultaneous deliveries. Burst commands emit
  sequentially; concurrent commands exercise concurrent delivery.
- Stale session locks produce recovery instructions rather than automatically
  reclaiming a possibly live process. Data can be reused with a new session.
- Mock AI counters cover one runner lifetime, not persistent production daily
  accounting. No external provider was called, and no production audit-store
  persistence or full Temps installation is claimed.
- Preview cookies are isolated from plugins; cookie-based plugin authentication
  and UI WebSocket upgrades require testing against a full Temps instance.
- No npm package publication is part of this change. The README explains how
  to use the source checkout before release.

### PR review corrections (2026-09-18)

- Project/environment fixture lists return all matching rows. Deployments retain the production default of 20, maximum of 100, zero-limit behavior, and newest-first ordering. Regression uses 125 records.
- Preview requests include resolved caller permissions, independently of plugin grants. Admin/reader snapshots are checked against the Rust source; compiled/source plugin tests cover valid permissions and rejection of spoofed reader permissions.
- Test file attribution and Bun file APIs corrected. Repository-wide source-attribution check passes.
- Verification: CLI typecheck/build, 1,186 tests (32 runner/model tests), and packed bunx native-plugin end-to-end test passed. AI remains mocked. Security review approved the permissions change.
