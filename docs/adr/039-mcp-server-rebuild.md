# ADR-039: MCP Server Rebuild — Axum-Native Endpoint + CLI Installer Wizard

**Status:** Proposed
**Date:** 2026-08-22
**Author:** David Viejo

---

## Context

### History

`@temps-sdk/mcp` was a separately-versioned npm package (workspace at `mcp/`,
~42 k lines, 31 tool-category modules, `@modelcontextprotocol/sdk` stdio
transport, hand-generated OpenAPI client). It was removed in commit
`2833d7fb6` (PR #355) with the explicit reason: "a second, separately-versioned
MCP server that needs its own tool coverage kept in sync with the API" — it had
drifted from the real API. The team standardised on skills + `bunx @temps-sdk/cli`
instead.

Separately, a Rust crate `temps-mcp` (using the `rmcp` SDK) was removed earlier
because `rmcp` 0.6.x had a Streamable-HTTP DNS-rebinding CVE (Dependabot high
advisory) with no other consumer in the workspace at the time.

The old package has been restored verbatim to `mcp/` and
`skills/temps-mcp-setup/` in branch `feat/mcp-server` as a baseline for
redesign.

Key facts about the baseline restored at `mcp/src/index.ts` and
`mcp/src/tools/index.ts`:

- Stdio-only transport; spawned by the AI client via `npx @temps-sdk/mcp`.
- 31 flat tool categories (projects, deployments, environments, domains,
  services, backups, monitors, containers, users, settings, api-keys, webhooks,
  audit, dns-providers, notifications, scans, custom-domains, errors,
  proxy-logs, dsn, ip-access, incidents, funnels, presets, platform,
  email-domains, email-providers, load-balancer, notification-prefs, analytics).
  See `mcp/src/tools/index.ts`.
- Category filtering via `--tools` CLI flag or `TEMPS_MCP_TOOLS` env var.
- No interactive installer — users hand-edited JSON config files per client.
- Authentication by env vars `TEMPS_API_URL` and `TEMPS_API_KEY` passed to
  the spawned process; the API key was not validated by the package itself.

### Why the same approach cannot be re-used

The structural problem that caused PR #355 is architectural, not cosmetic.  A
separately-versioned npm package that calls the REST API through a generated
client will drift the moment any endpoint, shape, or permission model changes
and the package is not updated in the same PR.  The team decided that
maintaining two surfaces — the REST API and an MCP translation layer — was not
acceptable.  That decision must be respected by the rebuild.

### Reference: PostHog's approach

PostHog's MCP server is a **remote, hosted HTTPS endpoint**
(`https://mcp.posthog.com/mcp`), not a separately-versioned package.  It lives
inside their live product, so it structurally cannot drift from the API.
Tool/feature selection is via a `?features=a,b,c` query parameter on that URL.
Their open-source installer wizard (github.com/PostHog/wizard) uses a clean
per-client adapter pattern (`MCPClient` abstract class, one subclass per
client: claude-code, claude-web/Desktop, cursor, codex, visual-studio-code, zed,
opencode) and `jsonc-parser` for comment-preserving JSON edits to each client's
config file.  For clients that speak native HTTP/SSE MCP (Claude Code, VS Code,
Cursor, Windsurf, Zed) the installer writes a
`{"url": "...", "headers": {"Authorization": "Bearer <key>"}}` entry directly.
For stdio-only paths it bridges through `npx -y mcp-remote@latest <url>
--header Authorization:...`.

The key structural insight from PostHog: because the MCP server is **part of the
product** rather than a consumer of its public API, it cannot go stale.

### Temps' specific constraints vs. PostHog

PostHog is a SaaS with one `mcp.posthog.com` endpoint.  Temps is self-hosted:
every Temps instance IS the product.  There is no shared `mcp.temps.sh` for
self-hosted users — the user's own Temps server (at `TEMPS_API_URL`) is the
canonical endpoint.  Temps Cloud instances (the managed offering) are the closest
analogue to PostHog's hosted case.  This shapes the transport decision directly:
"remote" for a Temps user means "my own Temps server", not a Temps-operated
multi-tenant URL.

### Forces

1. The rebuild must not recreate the drift problem.  Any MCP surface must live
   inside the product codebase — not in a separately-versioned package with its
   own OpenAPI client.
2. The workspace already contains 88 Rust crates.  Adding a crate is cheap; the
   bar is whether the crate has a well-bounded domain.
3. `rmcp` 0.6.x had a DNS-rebinding CVE.  Before taking a dependency on a newer
   version, the risk profile of that CVE must be evaluated in the specific
   deployment context.
4. CLAUDE.md §"Every new backend API endpoint needs CLI parity … that parity
   always lives in `apps/temps-cli/`" applies here.  The installer wizard is
   CLI-side tooling.
5. The repo already has a `mcp-servers` command in `apps/temps-cli/`
   (`apps/temps-cli/src/commands/mcp-servers/`) that manages external MCP server
   definitions for Temps agents connecting outward.  The new command must be
   named to avoid confusion with this.
6. The repo already has a `temps-ai-api-tools` crate that implements the
   propose-then-confirm write pattern for AI tool calls (`caller.rs`,
   `cli.rs`).  Any MCP write surface should follow the same pattern.
7. CLAUDE.md: security-sensitive changes require `security-auditor` agent
   sign-off.  The write-tool blast-radius and DNS-rebinding assessment both fall
   in scope.

---

## Decision

### 1. Transport and hosting — Axum-native endpoint in the `temps` binary

The MCP server is implemented as a new route group served directly from the
existing Temps Axum process.  It is **not** a separate npm package, a separate
Rust binary, or a standalone process.

**Structural reason:** An Axum route that calls the same services as the REST
API cannot drift from the API — they share the same service layer, entity types,
and permission model by construction.  This is the only architectural shape that
eliminates the drift problem rather than managing it.

**Transport protocol:** Streamable HTTP as specified by the MCP 2025-03-26
protocol revision, served over the same `TEMPS_TLS_ADDRESS` / `TEMPS_ADDRESS`
as the rest of the API, under the path prefix `/mcp`.  The endpoint supports
both `text/event-stream` (SSE) and `application/json` response modes, letting
HTTP-capable clients (Claude Code, VS Code, Cursor, Windsurf, Zed) connect
natively without a bridge process.  For stdio-only clients (older Claude Desktop
builds, any client that cannot speak HTTP MCP), the installer wizard bridges
through `npx -y mcp-remote@latest` exactly as PostHog does.

**`rmcp` dependency and DNS-rebinding assessment:**

The CVE that caused the original `temps-mcp` crate removal was a
DNS-rebinding advisory against `rmcp` 0.6.x's built-in standalone HTTP listener.
DNS rebinding exploits an unauthenticated local HTTP server: a malicious web
page resolves a domain to `127.0.0.1` after the browser's DNS cache entry
expires, then makes cross-origin requests that the browser's same-origin policy
would otherwise block.

That attack surface **does not exist** for an Axum-native route:

- The route is part of the existing Axum server, which already enforces CORS and
  Origin validation on all routes.
- Every request requires a `Authorization: Bearer <api-key>` header.  A
  DNS-rebinding attack from a browser cannot supply an API key it does not
  already hold; if it holds the API key, the user's Temps instance is already
  compromised by some other means.
- The route does not run as a standalone listener — it has no separate bind
  address, no separate socket, and inherits all of the API server's TLS and
  network configuration.

The rmcp CVE is therefore not a relevant risk for this design.  The decision is
to **hand-roll the MCP JSON-RPC handler in Axum** without taking any rmcp
dependency, for two independent reasons: (a) it avoids the CVE question
entirely, and (b) the MCP protocol's server-to-client surface for this use case
is small (four request types: `initialize`, `tools/list`, `tools/call`,
`notifications/cancelled`) and the implementation does not warrant a framework.
If rmcp 0.7+ (or a successor) receives an explicit RustSec "patched" advisory
for the DNS-rebinding issue, the decision can be revisited, but it is not needed
for the current design.

**Affected crate:** A new crate `temps-mcp-server` (not to be confused with the
removed `temps-mcp`) is introduced under `crates/`.  It owns the MCP
route handler, tool registry, and SSE/streaming response construction.  It takes
service `Arc<T>` references from `AppState` the same way every other handler
crate does and uses no shared error types.  It registers via `TempsPlugin`.

The route group is:
```
POST /mcp        -- main MCP endpoint (Streamable HTTP)
GET  /mcp        -- SSE session upgrade (for SSE-mode clients)
DELETE /mcp      -- session termination
GET  /mcp/tools  -- unauthenticated capability probe (tool group list only, no API call, see §4)
```

### 2. Tool taxonomy and configurability

**Regrouping:** The 31 flat categories from the baseline (`mcp/src/tools/index.ts`)
are reorganised into 7 top-level groups, modelled on PostHog's curated
"Data & Analytics" / "Development Tools" top-level organisation.  The groups
proposed below are a direct mapping of the 31 categories:

| Group key | Included categories | Human label |
|---|---|---|
| `deployments` | projects, deployments, environments, presets | Deployments & Projects |
| `infrastructure` | services, containers, load-balancer, scans | Infrastructure |
| `networking` | domains, custom-domains, dns-providers, ip-access | Networking & Domains |
| `data` | backups, dsn | Databases & Backups |
| `observability` | monitors, incidents, errors, proxy-logs, funnels, analytics | Observability |
| `notifications` | notifications, notification-prefs, webhooks, email-domains, email-providers | Notifications |
| `platform` | users, settings, api-keys, audit, tokens, platform | Platform & Access |

Each group contains both read and write tools.  Write tools within a group are
controlled independently (see below).

**Permission layering — two independent gates:**

Gate 1 — **API key scope (existing system).**  The Temps API key presented in
the `Authorization` header already carries scoped permissions (see
`crates/temps-auth`).  A tool call that requires `DeploymentsCreate` but whose
key only has `DeploymentsRead` is rejected at the service layer with a 403,
exactly as the equivalent REST call would be.  No new permission enforcement is
added here; the existing system already handles it.

Gate 2 — **Tool group filter (new, analogous to PostHog's `?features=`).**
The MCP endpoint accepts an optional `?groups=deployments,observability`
query parameter on the initial connection URL.  When present, only the listed
groups' tools are registered in the `tools/list` response.  When absent, all
groups are listed (subject to Gate 1).  The installer wizard writes the
`?groups=` param into the client's config at install time, so the user picks
their groups once during setup rather than at every AI session.

**Write-tool opt-in (new).**  By default, only read tools are registered.
Write tools (those whose implementation calls a non-idempotent API method) are
suppressed unless the connection URL includes `?write=1`.  This is independent
of Gate 1 and Gate 2.  The installer wizard asks the user whether they want
write tools and emits the flag accordingly.  Read-only mode is the default
because a Temps instance controls real infrastructure (deployments, DNS,
databases) and an AI assistant with unconstrained write access to that
infrastructure is a meaningful blast-radius risk even when operating correctly.

**Write tool call flow — propose-then-confirm.**  When `?write=1` is active,
write tool calls follow the same pattern as `crates/temps-ai-api-tools`
(`caller.rs`, the `temps_write` propose-then-confirm path documented in
`crates/temps-ai-api-tools/src/lib.rs`):

1. The MCP tool call returns a structured "proposal" result describing the
   intended mutation, its parameters (recursively redacted for display), and a
   unique proposal token.
2. A separate `confirm_action` tool (always registered when `?write=1`) accepts
   the proposal token and executes the mutation after verifying it is not
   replayed and has not expired.
3. The MCP client presents the proposal text to the user.  If the user approves,
   the client calls `confirm_action`.  If not, the token expires without effect.

This means that even with `?write=1`, no infrastructure change happens without
an explicit user confirmation in the MCP client UI.  The pattern is identical to
the existing AI chat write path; no new approval concept is introduced.

### 3. Installer wizard — `bunx @temps-sdk/cli mcp add <client>`

The installer lives entirely in `apps/temps-cli/src/commands/mcp/`.  This
follows the CLAUDE.md rule that "ALL new backend-API-facing client tooling lives
in `apps/temps-cli` (`@temps-sdk/cli`), never a separate npm package or a Rust
CLI subcommand."

The command namespace is `mcp` (not `mcp-servers`, which already exists for the
unrelated feature of registering external MCP servers that Temps agents connect
to outbound).  Sub-commands:

```
bunx @temps-sdk/cli mcp add <client>   # configure a local MCP client to point at this Temps instance
bunx @temps-sdk/cli mcp remove <client>
bunx @temps-sdk/cli mcp status         # show which clients have Temps MCP configured
```

where `<client>` is one of: `claude-code`, `claude-desktop`, `codex`, `cursor`,
`vscode`, `windsurf`, `zed`.

**Client adapter pattern.**  Port PostHog's `MCPClient` abstract class pattern
directly.  Each supported client is a TypeScript class implementing:

```typescript
abstract class McpClientAdapter {
  abstract getConfigPath(): string
  abstract getServerPropertyName(): string
  abstract isServerInstalled(): boolean
  abstract addServer(entry: McpServerEntry): void
  abstract removeServer(): void
  abstract isClientSupported(): boolean
  supportsNativeHttp(): boolean  // true for claude-code, vscode, cursor, windsurf, zed
}
```

**Config editing.**  Use `jsonc-parser` (already in the npm ecosystem; add to
`apps/temps-cli/package.json`) for comment-preserving JSON edits to each
client's actual config file.  Installs are idempotent: diff the existing entry
before writing, and skip if identical.

**Transport selection per client.**

| Client | Native HTTP MCP | Bridge needed |
|---|---|---|
| Claude Code | yes — `~/.claude.json` `mcpServers` map | no |
| VS Code | yes — `settings.json` `github.copilot.chat.mcp.servers` | no |
| Cursor | yes — `~/.cursor/mcp.json` | no |
| Windsurf | yes — `~/.codeium/windsurf/mcp_settings.json` | no |
| Zed | yes — `~/.config/zed/settings.json` `context_servers` | no |
| Claude Desktop | stdio only | `mcp-remote` bridge |
| Codex | stdio only | `mcp-remote` bridge |

For native-HTTP clients the installer writes:

```json
{
  "url": "https://<TEMPS_API_URL>/mcp?groups=<selected>&write=<0|1>",
  "headers": { "Authorization": "Bearer <api-key>" }
}
```

For stdio-only clients it writes:

```json
{
  "command": "npx",
  "args": [
    "-y", "mcp-remote@latest",
    "https://<TEMPS_API_URL>/mcp?groups=<selected>&write=<0|1>",
    "--header", "Authorization:Bearer <api-key>"
  ]
}
```

**Wizard flow.**

1. Check `TEMPS_API_URL` and an available API key (from the CLI's auth store or
   `--api-key` flag).
2. Probe `/mcp/tools` (the unauthenticated capability endpoint) to confirm the
   instance is running the new MCP server.
3. Prompt for tool groups to enable (multi-select, defaulting to all read
   groups).
4. Prompt whether to enable write tools (default: no).
5. Check whether the target client is installed on this machine
   (`isClientSupported()`).
6. Show a diff of the proposed config change.
7. Write the config if confirmed.
8. Print the next step (e.g. "Restart Claude Desktop to pick up the change").

### 4. Security

This section is written to inform the mandatory `security-auditor` review.

**Authentication.** Every MCP request other than `GET /mcp/tools` requires a
valid `Authorization: Bearer <api-key>` header.  The API key is validated by the
existing `temps-auth` middleware; no new auth path is introduced.  `GET /mcp/tools`
returns only the list of group names and human labels — no tool schemas, no API
call, no infrastructure data — and is safe to leave unauthenticated so that the
installer wizard can probe whether the MCP endpoint exists without requiring an
API key at probe time.

**DNS rebinding.** Assessed in §1.  Not a relevant risk for an authenticated
Axum route with existing CORS/Origin enforcement.

**Unauthenticated tool listing.** The full `tools/list` MCP response (including
input schemas) is only served to authenticated callers.  The unauthenticated
`GET /mcp/tools` endpoint returns group metadata only.

**Rate limiting.** Inherited from the existing Axum API middleware.  No new rate
limiting is added; the MCP endpoint sits behind the same per-key request budget
as the REST API.  If the existing limiter does not cover the MCP path, that gap
should be closed before the feature is enabled on Temps Cloud.

**Blast radius — write tools.**  Write tools are opt-in (`?write=1`), suppressed
by default.  Even when opted in, every write tool follows the propose-then-confirm
pattern: no mutation executes without an explicit user confirmation token in the
MCP client.  This mirrors the existing `temps_write` / `confirm_action` pattern
in `crates/temps-ai-api-tools` and extends it to the MCP surface.

**API key minimisation.** The installer wizard recommends that the user creates a
dedicated, scoped API key for MCP access rather than reusing an admin key.  The
prompt step that asks for the key includes a link to the API keys settings page.
The generated config file embeds the key in plaintext in the client's config file
(this is unavoidable for client-side config files that are read by the client
process); the user should be warned that the key should be scoped to read-only if
write tools are not enabled.

**Proposal token expiry and replay prevention.** Write proposal tokens issued by
`temps-mcp-server` must expire (suggested: 5 minutes) and must be single-use
(mark as consumed on first successful `confirm_action` call).  This is the same
requirement as `temps-ai-api-tools/src/caller.rs` where it notes "The confirm
endpoint … a call cannot be replayed by the fallback dispatcher."

**Security auditor sign-off required** before this feature is enabled on Temps
Cloud or documented as production-ready.  Scope of review: authentication bypass
paths on `/mcp`, rate limiting coverage, proposal token replay, and the API key
embedding guidance in the wizard.

### 5. Migration and rollout

**`@temps-sdk/mcp` npm deprecation.**  Commit `2833d7fb6` recommended running:

```
npm deprecate @temps-sdk/mcp "@temps-sdk/mcp is no longer maintained; use bunx @temps-sdk/cli instead"
```

It is not confirmed whether this was ever executed.  Before this ADR's feature
ships, a maintainer with npm publish access to `@temps-sdk/mcp` must verify the
deprecation status and publish it if it has not been run.  This is a manual,
one-time operational step, not a code change.

**`temps-mcp-setup` skill.**  The skill at `skills/temps-mcp-setup/` described
the manual stdio setup process.  It should be removed from the worktree once the
new installer wizard (`bunx @temps-sdk/cli mcp add`) is merged and documented.

**`mcp/` workspace directory.**  The restored baseline at `mcp/` in this
worktree is not to be published.  It exists as a reference for the existing tool
implementations.  Once `temps-mcp-server` is implemented and the tool coverage
is validated, the `mcp/` directory should be deleted in the same PR that
introduces `temps-mcp-server`, so it does not linger as a confusing second
surface.

**Feature flag / gradual rollout.**  The MCP endpoint (`/mcp`) is introduced
behind the Temps feature flag system (ADR-034).  Self-hosted operators can
enable it by setting the flag; it is off by default until the security review
is complete.  On Temps Cloud the flag is toggled on after the security review.

---

## Consequences

### Positive

- The drift problem is eliminated by construction.  `temps-mcp-server` calls
  the same service layer as the REST API; the two surfaces cannot diverge.
- No new separately-versioned package to maintain.  The MCP route lives in the
  monorepo and is updated as part of normal API development.
- Native HTTP MCP transport eliminates the stdio spawning overhead and the
  `mcp-remote` bridge for all major clients except Claude Desktop and Codex.
- The propose-then-confirm write pattern is extended to a new surface without
  inventing a new pattern — it reuses `temps-ai-api-tools` concepts.
- The installer wizard gives users a one-command path (
  `bunx @temps-sdk/cli mcp add claude-code`) rather than manual JSON editing,
  across all seven supported clients.
- The seven-group taxonomy is more discoverable than 31 flat categories; users
  can pick "Observability" without knowing whether they want `monitors`,
  `incidents`, `errors`, and `proxy-logs` individually.

### Negative

- A new Rust crate (`temps-mcp-server`) adds to the workspace.  It has no
  independent release cycle; it is part of the main binary.
- Hand-rolling MCP JSON-RPC means the team owns the protocol implementation.
  The MCP spec is small and stable, but any spec revision must be tracked.
- The `mcp-remote` bridge is still needed for Claude Desktop and Codex; those
  users must have Node.js available.
- The `?write=1` / propose-then-confirm flow adds latency to write operations
  compared to a direct REST call.  This is intentional (safety over speed) but
  may frustrate power users.

### Risks

- The security review could identify issues in the authentication or blast-radius
  design that require architectural changes before the feature can ship.
- The npm deprecation of `@temps-sdk/mcp` may not have been executed.  Existing
  `@temps-sdk/mcp` installs will continue to work against an old API surface
  until the user upgrades; there is no forced migration.
- `mcp-remote` is an npm package maintained by the MCP project; it is a runtime
  dependency for Claude Desktop and Codex users.  Its versioning and availability
  are outside Temps' control.

---

## Alternatives Considered

### Option A: Republish the restored `mcp/` npm package with a sync CI job

Keep `@temps-sdk/mcp` as a separately-versioned package but add a CI check that
fails if the MCP tool list diverges from the OpenAPI spec.

- Pros: No Rust crate to write; familiar TypeScript.
- Cons: Recreates the exact problem that caused PR #355.  A CI check that
  prevents *detecting* drift is not the same as eliminating drift structurally.
  The package still has its own version, release cycle, and OpenAPI client.

Rejected: the structural problem is not solved.

### Option B: Rebuild the MCP server as a separate TypeScript service in the monorepo

A Node.js/Bun process adjacent to the Rust binary that proxies the REST API and
exposes an MCP endpoint.

- Pros: TypeScript; reuses the generated SDK client; familiar to frontend devs.
- Cons: The generated SDK client is a consumer of the public API, so drift is
  still possible if the client is not regenerated.  Introduces a second process
  with its own lifecycle, health, and restart semantics.  The CLAUDE.md rule
  against separate packages would require it to live in `apps/`, which is the
  right location only for client tooling, not a server.

Rejected: does not eliminate drift; adds process complexity.

### Option C: Use `rmcp` as the Rust MCP SDK

Take a dependency on `rmcp` 0.7+ and use it to implement the MCP server.

- Pros: Less protocol boilerplate.
- Cons: The DNS-rebinding CVE was against `rmcp`'s standalone HTTP listener; we
  do not know whether 0.7+ has an explicit RustSec "patched" advisory at the
  time of writing.  Hand-rolling the four relevant MCP request types in Axum is
  straightforward and avoids the dependency entirely.  If the rmcp advisory is
  formally resolved in a future version, this can be revisited.

Rejected: dependency risk outweighs the boilerplate saved.

### Option D: Serve MCP only on Temps Cloud; self-hosted gets the CLI only

Run a shared `mcp.temps.sh` endpoint on Temps Cloud (analogous to
`mcp.posthog.com`) and tell self-hosted users to use `bunx @temps-sdk/cli`
directly.

- Pros: Simplest possible Cloud surface; no self-hosted server changes.
- Cons: Self-hosted users — the majority of the target audience — get no MCP
  integration.  Inconsistent product between Cloud and self-hosted.  Contrary
  to "make Temps the obvious choice for anyone who wants to own their deployment
  infrastructure."

Rejected: leaves self-hosted users without MCP support.

---

## Implementation Notes

- **Affected crates:** new `crates/temps-mcp-server`; minor changes to
  `crates/temps-routes` to register the new route group; `apps/temps-cli`
  for the `mcp add/remove/status` commands.
- **Migration needed:** yes — delete `mcp/` workspace, delete
  `skills/temps-mcp-setup/`, verify npm deprecation.
- **Breaking changes:** no — the new endpoint is additive.  Existing skills and
  CLI commands are unaffected.
- **Security auditor sign-off required** before Cloud enablement.
- **Feature flag:** use ADR-034 flag system; default off until security review
  is complete.
- **Naming disambiguation:** the new command is `mcp` (not `mcp-servers`).
  The `mcp-servers` command at `apps/temps-cli/src/commands/mcp-servers/`
  manages external MCP servers for Temps agents connecting outward and is
  unrelated.
- **Write proposal token implementation:** follow the same ephemeral-token
  pattern as `crates/temps-ai-api-tools/src/caller.rs`; implement in
  `temps-mcp-server`, not in `temps-ai-api-tools` (separate domain).
- **Open operational item:** a maintainer with npm publish access must confirm
  and if necessary execute:
  `npm deprecate @temps-sdk/mcp "@temps-sdk/mcp is no longer maintained; use bunx @temps-sdk/cli mcp add instead"`
