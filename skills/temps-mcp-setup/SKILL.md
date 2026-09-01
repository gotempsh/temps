---
name: temps-mcp-setup
description: |
  Configure Temps as an MCP (Model Context Protocol) server so AI assistants can interact with a Temps instance directly -- listing/inspecting projects and deployments, and (when write mode is enabled) triggering deployments with human confirmation. Use when the user wants to: (1) Set up the Temps MCP server, (2) Connect Claude Code/Desktop, Codex, Cursor, VS Code, Windsurf, or Zed to Temps, (3) Add Temps tools to an AI assistant, (4) Test the MCP wizard locally, (5) Manage multiple Temps MCP connections (dev, staging, prod, or several local dev slots) side by side. Triggers: "temps mcp", "configure temps tools", "add temps to claude", "temps ai assistant", "mcp server setup", "mcp add", "test the mcp wizard".
---

# Temps MCP Setup

Temps serves MCP (Model Context Protocol) directly from the `temps` binary itself
(ADR-039, `crates/temps-mcp-server`) -- there is no separate package to install or
keep up to date. The MCP endpoint calls the same service layer as the REST API, so
it cannot drift from it the way the old standalone `@temps-sdk/mcp` npm package did
(removed in PR #355 for exactly that reason -- do not reinstall it or point a
client at it).

Installer wizard lives in `apps/temps-cli/src/commands/mcp/`
(`bunx @temps-sdk/cli mcp add|remove|status`).

## 1. Enable the MCP server (operator, one-time per instance)

Off by default (`AppSettings.mcp_server.enabled = false`), so a fresh install never
exposes it unconfigured. Turn it on as an admin:

```bash
bunx @temps-sdk/cli mcp enable
```

If you're not logged in yet, `mcp enable` offers to run the device-flow login
inline (see [Auth model, precisely](#2-configure-your-ai-client-per-user-per-client)
below) -- no separate `temps login` step required first.

`mcp enable` does the same GET-modify-PUT round-trip against `/api/settings` the
handler requires (it takes the **full** `AppSettings` object, not a partial patch),
merging onto whatever is already fetched so it can never clobber another admin's
settings. `mcp disable` reverses it.

Verify it took effect, or check status any time without re-running enable:

```bash
bunx @temps-sdk/cli mcp status
# "This instance: <check> enabled (http://localhost:8080)" or "<bullet> disabled (...)"
```

Or probe the endpoint directly (no auth needed for this one):

```bash
curl -s http://localhost:8080/mcp/tools
# {"groups":[{"key":"deployments","label":"Deployments & Projects"}, ...]}
# A 404 here means the flag is still off (or this instance predates MCP support).
```

## 2. Configure your AI client (per user, per client)

```bash
bunx @temps-sdk/cli mcp add <client>
```

`<client>` is one of: `claude-code`, `claude-desktop`, `codex`, `cursor`, `vscode`,
`windsurf`, `zed`.

**Auth model, precisely:** `mcp add`/`mcp enable`/`mcp disable` need you logged
into the CLI so the wizard can mint an API key on your behalf -- that login has
nothing to do with MCP itself. If you're not already logged in, these commands
detect that and offer to run the device-authorization flow right there (prompts
"Log in now?", then "Temps server URL", defaulting to your current config) rather
than erroring out and making you run `temps login` as a separate step first. Under
the hood it's the same flow `temps login` uses (`/auth/cli/device/start` +
`/auth/cli/device/poll`, server-authoritative polling, no code to type, browser
approval). In `--yes` (non-interactive) mode this inline offer is skipped --
pass `--api-key` or ensure a context is already logged in. What actually gets
written into the AI client's config either way is a plain, long-lived
`Authorization: Bearer <api-key>` header -- the same static-bearer-token pattern
PostHog's own MCP wizard uses, and one of the two auth patterns the MCP HTTP
transport spec supports (the other being full OAuth 2.1 + dynamic client
registration, which most of these 7 clients don't yet implement for remote MCP
servers anyway).

The wizard will:
1. Probe `/mcp/tools` to confirm the instance has MCP enabled (see step 1). If it
   404s, it tells you so and stops -- it will not silently write a broken config.
2. Ask which tool groups to enable (default: all).
3. Ask whether to enable write tools (default: **no** -- read-only). Write tools
   still require human confirmation per call even when enabled (see below).
4. Offer to create a dedicated, scoped API key for MCP access, recommended over
   reusing a broader existing credential. The new key's role is `role_type:
   'reader'` when write mode is off, `'user'` when it's on (the `user` role
   carries `DeploymentsWrite`; `reader` does not).
5. Write the client's config file (or, for Claude Code/Codex, shell out to their
   own `claude mcp add` / `codex mcp add` so their config format is never
   hand-maintained here).

Other subcommands:

```bash
bunx @temps-sdk/cli mcp status         # which clients on this machine have Temps configured
bunx @temps-sdk/cli mcp remove <client>
```

Restart the AI client after running `mcp add` -- most clients only read MCP config
at startup.

## 3. Testing the wizard locally, end-to-end

Bring up a local instance with the `start-temps` skill first (or reuse one you
already have running), noting its **slot** -- every non-zero slot has its own
ports, database, and login, so treat the printed `web`/`api` URLs as this
instance's identity for the rest of this section.

Fastest path to a working test without the interactive prompts: `mcp add --yes`
with an explicit `--api-key`, pointed at the slot's API URL via `TEMPS_API_URL` (or
`--target-context`, if you've already run `temps login` against that instance and
saved a context).

```bash
# One-time per slot: enable the flag (step 1) and mint a throwaway admin key
# (or use the wizard's own key-creation step interactively instead).

TEMPS_API_URL=http://localhost:<8080+slot*10> bunx @temps-sdk/cli mcp add claude-code \
  --api-key <key> --yes
```

Then drive the protocol directly with `curl` -- this is the fastest way to verify
the server side without depending on any particular AI client being installed:

```bash
API=http://localhost:<8080+slot*10>
KEY=<your-api-key>

# Capability probe (no auth)
curl -s "$API/mcp/tools"

# initialize
curl -s "$API/mcp" -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}'

# tools/list -- add ?write=1 to the $API/mcp URL to also see trigger_deployment/confirm_action
curl -s "$API/mcp" -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'

# tools/call
curl -s "$API/mcp" -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_projects","arguments":{}}}'
```

If `list_projects` returns `[]`, there's nothing to look at yet -- create a project
via the normal REST API or the web UI first (`create_project` is not an MCP tool;
see Current Coverage below).

**Testing the write flow** (propose-then-confirm): call `trigger_deployment` on the
`?write=1` connection with a real `project_id`/`environment_id`, note the
`_proposal_token` in the response, then call `confirm_action` with that token
within 5 minutes. Confirm it only works once -- replaying the same token must
return a `Proposal token not found or already used` error. If you have DB access,
confirm the audit row landed: `SELECT * FROM audit_logs WHERE operation_type =
'MCP_DEPLOYMENT_TRIGGERED' ORDER BY id DESC LIMIT 1;`.

**Testing an actual AI client end-to-end:** after `mcp add <client>` and a restart,
ask it something that requires a tool call ("list my Temps projects") and confirm
it actually invokes `list_projects` rather than hallucinating an answer -- most
clients show a tool-call approval prompt or a visible "using tool" indicator you
can check against the request actually hitting your instance's logs.

## 4. Managing multiple installations

You will commonly have more than one Temps instance you want an AI client talking
to at once -- several local dev slots, or dev + staging + prod. Each is a fully
independent MCP connection: a distinct URL, a distinct API key, and (for clients
that support named servers) a distinct entry in that client's config.

- **Run `mcp add <client>` once per instance you want configured.** The wizard
  does not currently namespace by instance -- it writes to the single `temps`
  entry key most clients use (`mcpServers.temps` / `context_servers.temps` /
  etc.), so adding a second instance for the *same* client **overwrites** the
  first entry rather than adding a second one. If you need two Temps connections
  live in the same client simultaneously today, hand-edit the client's config
  file after running the wizard once, duplicating the `temps` entry under a
  second key (e.g. `temps-staging`) with that instance's URL/key -- the wizard
  itself doesn't offer this yet.
- **`bunx @temps-sdk/cli mcp status`** only reports whether *a* Temps entry exists
  per client, not which instance it points at -- check the URL inside the config
  file directly if you've hand-edited it, or `bunx @temps-sdk/cli mcp remove
  <client>` and re-run `mcp add` when switching which instance a client talks to.
- **Scope API keys per instance, not per person.** Each `mcp add` run against a
  different instance should mint its own key on that instance (step 2.4) rather
  than reusing one key across instances -- keys don't work cross-instance anyway
  (each instance has its own user/key table), but it also keeps revocation
  scoped: pulling a compromised or no-longer-needed key from one instance's
  Settings -> API Keys doesn't touch the others.
- **Local dev slots are the common case for this repo.** Each `start-temps` slot
  is already a fully separate instance (own port, own DB, own admin login) --
  point `TEMPS_API_URL` at the specific slot you're testing (see the `start-temps`
  skill for the port formula) and mint a key on that slot. Don't reuse a key
  minted on slot 0 against slot 3's API URL; it will 401 (different DB, different
  users table).
- **Read-only vs. write connections are also separate "installations" in
  practice.** If you want both a safe read-only Temps connection for everyday use
  and a write-enabled one for a specific deployment-management session, that's
  two `mcp add` runs (two keys, two config entries) rather than one -- the
  wizard's write-mode question is per-connection, not a runtime toggle.

## Tool groups

Tools are organized into 7 groups, selectable via the wizard or the connection
URL's `?groups=` param:

| Group | Contents |
|---|---|
| `deployments` | projects, deployments, environments, presets |
| `infrastructure` | services, containers, load-balancer, scans |
| `networking` | domains, custom-domains, dns-providers, ip-access |
| `data` | backups, dsn |
| `observability` | monitors, incidents, errors, proxy-logs, funnels, analytics |
| `notifications` | notifications, notification-prefs, webhooks, email-domains, email-providers |
| `platform` | users, settings, api-keys, audit, tokens, platform |

**Current coverage:** only `platform` (`list_projects`, `get_project`) and
`deployments` (`list_deployments`, and the write tools `trigger_deployment` /
`confirm_action`) have real tools implemented so far. The other five groups exist
in the taxonomy but have no tools registered yet -- `tools/list` omits them until
they're ported from the old `mcp/src/tools/*.ts` reference implementations (kept
on this branch for reference, not published). Notably, there is **no
`create_project` MCP tool yet** -- create projects via the REST API or web UI, then
use MCP to list/inspect/deploy them.

## Write tools: propose-then-confirm

When a client is configured with write mode on, calling a write tool (e.g.
`trigger_deployment`) does **not** execute immediately. It returns a proposal
describing the intended action and a short-lived (5 minute), single-use token. The
AI client shows this to you; if you approve, it calls `confirm_action` with that
token, which is the only path that actually executes the mutation. Declining, or
letting the token expire, does nothing. Every successful `confirm_action` write is
audit-logged (`MCP_DEPLOYMENT_TRIGGERED`) with the calling user, IP, and user
agent, same as the equivalent REST endpoint.

This means enabling write mode does not hand an AI assistant unsupervised control
of your infrastructure -- every mutation still needs an explicit confirmation step
in the conversation.

## Authentication

Every request except `GET /mcp/tools` requires `Authorization: Bearer <api-key>`,
validated by the same auth path as the REST API. A key's existing scoped
permissions apply normally -- an MCP tool that needs `DeploymentsWrite` is
rejected exactly like the equivalent REST call would be if the key lacks it.
`?groups=` / `?write=1` only narrow what a call *could* reach; they never grant
permissions the key doesn't already have.

## Troubleshooting

**`mcp add` reports "this instance does not support MCP":**
- The feature flag is off -- see step 1.
- You're pointed at the wrong `TEMPS_API_URL` (check `bunx @temps-sdk/cli status`).

**Tools not appearing in the AI client after `mcp add`:**
- Restart the client completely.
- Run `bunx @temps-sdk/cli mcp status` to confirm the config was actually written.
- For Claude Code/Codex, run `claude mcp list` / `codex mcp list` directly to check
  their own registry.

**Write tool calls fail with a permission error even though write mode is on:**
- `?write=1` only controls whether write tools are *listed*; the API key still
  needs the underlying permission (e.g. `DeploymentsWrite`) to actually call one
  -- that check happens at call time, the same as the equivalent REST endpoint.

**A tool call returns "unknown tool":**
- Check which groups are active for this connection (`?groups=` in the config
  URL) -- a tool outside the active groups is not registered at all, not just
  hidden.

**Local testing hits a "preview gateway: forbidden" error instead of your MCP
response:** unrelated to MCP -- the global `temps-preview-gateway` Docker
container's host port mapping can coincidentally collide with a dev slot's HTTP
port. Hit the machine's real LAN IP (`ifconfig`, not `localhost`/`127.0.0.1`)
instead; the `temps serve` process binds `0.0.0.0`, so it's reachable on any
interface even when `127.0.0.1:<port>` is claimed by the gateway container.
