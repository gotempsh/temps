# ADR-112: A project-focused console

**Status:** Proposed — 2026-11-23. Step 1 (project-scoped navigation) is
implemented in the same PR as this note; steps 2 to 4 are for the maintainer
to decide.

## Context

The console has 197 page components under `web/src/pages` and many more
routed views inside them. It grows by one page per feature. The direction
being explored is:

- a few everyday screens;
- everything else configured by hand, or by an agent that proposes a change
  for approval, with every change on the timeline;
- users pin panels into views of their own.

Operator feedback from daily use reframes the problem. The number of pages is
not the pain. **Scope** and **several ways to do one thing** are:

1. Opening a project (`/p/:slug`) left the server-wide sidebar in place. The
   sidebar only changed inside a service (`/projects/:slug`).
2. Domains, databases and backups were only reachable as server-wide lists,
   though most operators think of them per project.
3. Inside a service the sidebar was a different one. Its Databases page then
   listed every database on the server, with the service's own mixed in.
4. Several entry points reach one view, and views show things outside the
   current focus.

The same operator wants pinned views built from a mix of panels. For AI, they
would rather connect their own agent harness through the API or MCP than use
a built-in chat.

Hierarchy, for the rest of this note: a **project** is an API
`project_group` (`/p/:slug`); a **service** is an API `project`
(`/projects/:slug`), usually inside one project (`web/src/lib/terminology.ts`).

## Decision

### 1. Project-scoped navigation (implemented)

The URL alone decides the sidebar (`resolveConsoleScope` in
`web/src/lib/console-scope.ts`):

| Scope | URL | Sidebar |
|---|---|---|
| Global | anything else | Server-wide administration: worker nodes, users, plugins, every database, domain and backup on the server. Shown only outside a project. |
| Project | `/p/:slug/*` | "All projects" and a project switcher at the top, then Overview, Services, Observe (Deployments, Logs, Errors, Traces, Analytics), Resources (Domains, Databases, Backups), Configure (Shared variables, Settings). |
| Service in a project | `/projects/:slug/*` | The same project sidebar, with the service open as a nested level showing its sections. |
| Standalone service | `/projects/:slug/*` | The service's own menu, with the same "All projects" header. |

Project pages show only that project's resources, with one exception: the
database source of project Logs has no project filter yet, so it shows
database logs from the whole server and says so ("Showing all on this
server"; see the table below). Each project page has a single explicit link
to the server-wide list ("All databases on this server").
Nothing became unreachable. Every server-wide list is still in the global
sidebar. Every project page is a ⌘K destination ("Storefront · Databases").
Old URLs redirect.

The PR is frontend-only. Where the API had no project filter, a project list
is the union of per-service endpoints the server already scopes. The fan-out
is bounded (25 services) and the page says when a list is partial. The
console never filters a server-wide list in the browser. Backend follow-ups
that would replace the fan-out:

| List | Today | Needed |
|---|---|---|
| Databases | `GET /external-services/projects/{project_id}` per service | `GET /external-services?project_group_id=` (paginated) |
| Domains | `GET /projects/{project_id}/custom-domains` per service | `GET /custom-domains?project_group_id=` (no cross-service custom-domain list exists) |
| Deployments | `GET /projects/{id}/deployments` per service, 10 each | `GET /deployments?project_group_id=` (paginated, newest first) |
| Backups | `GET /backups/external-services/{service_id}/backups` per database | `GET /backups?project_group_id=` |
| Errors | `GET /error-groups?project_id=` (one service) | `project_group_id` or `project_ids[]` |
| Traces, analytics | one `project_id` | `project_group_id` |
| Logs, database source | no project filter; labelled "Showing all on this server" | a database (service) filter |
| Project timeline | `audit_logs` has no project column | `project_group_id` on audit entries |

Logs already accepts a list of services, so project Logs covers every service
at once. Errors, Traces and Analytics take one service, so in a project their
service picker lists only that project's services.

### 2. Everyday screens and secondary pages (proposed)

This is based on the Part 1 inventory. Everyday means it is in the project
sidebar and opened most days. Secondary means it is reached from a link in an
everyday screen, from ⌘K, or from Settings.

| Everyday (project sidebar) | Secondary |
|---|---|
| Project overview / map (services, routes, databases, last deploy) | Environments, Build & deploy, Security & access, Automation & integrations |
| Logs (all services of the project) | Errors › Source maps, Inbound filters, Attachments, Ownership, SDK health |
| Analytics overview | Analytics › Funnels, Surveys, Journey, Revenue, AI visitors, Traffic filters |
| Errors (issues) | Monitoring › Cron jobs, Check-ins, Durable workflows |
| Databases (the project's, with browse) | Traces › AI traces |
| Deployments | Domains, Backups, Shared variables, Settings |

Everything under Settings, worker nodes, plugins, vault, git connections, DNS
providers and delivery profiles stays global administration.

### 3. Pinned views / custom dashboards (proposed)

**Storage.** A new `views` table: `id`, `owner_user_id`, optional
`project_group_id` (null means a server view), `name`, `layout` (typed JSON
with a schema version), `shared` (none / project members / everyone with read
access to the scope), and timestamps. It generalises the per-service
`metric_dashboards` (`project_id`, `name`, `layout`). Those migrate into views
with one panel kind (`metric_chart`), and their URLs redirect. Views go
through the normal write path, so they are audit-logged and paginated like any
list.

**Panel contract.** A panel is `{ kind, version, scope, params }`:

- `kind` names a registered renderer: `metric_chart`, `log_stream`,
  `deploy_list`, `error_rate`, `error_list`, `analytics_kpi`, `uptime`,
  `query` (a saved database query), or a plugin's own kind (`plugin:<id>/<kind>`).
- `scope` is a project and/or a service ID list. The panel reads through the
  same public API a user would, with the viewer's permissions. A shared view
  shows a viewer only what they may read.
- `params` is validated by the kind's schema, published in OpenAPI.
- Plugins register kinds through `@temps-sdk/console-kit`, next to the
  existing `navItems` / `settingsNavItems`. A kind declares its params schema,
  its default size and an empty state.

**"Ask an agent to build a view".** An agent (through the MCP server or the
API) can only compose existing panel kinds into a view: it writes a view
document, never code or queries outside a kind's params. The result is a
proposal (step 4) that the user previews and accepts. An accepted view is an
ordinary view, editable by hand.

### 4. Agent-proposed changes with approval (proposed)

- **Proposal object.** It replaces the in-memory, 5-minute
  `temps-mcp-server` `ProposalStore` with a persisted `change_proposals`
  record: `project_group_id`, `service_id?`, author (user, API key or agent
  identity), a typed `change` (the same request body the hand-editing
  endpoint takes), a rendered diff against current state, and
  `status`: open / approved / rejected / applied / failed / stale.
- **Timeline.** Proposals and their outcomes appear on the project timeline
  (audit entries gain `project_group_id`). Approving needs the same
  permission the change itself needs, and an explicit OK. Nothing is applied
  silently. A proposal goes stale when the resource changes under it
  (compare `updated_at`), and is not applied blindly.
- **Your own harness first.** MCP tools and the API can `propose_change`,
  `list_proposals` and `get_proposal`. They cannot approve their own
  proposals. Connecting a harness is set up from Settings › MCP server, as it
  is today.
- **Built-in chat is optional.** It works against a local model through the
  AI gateway. It uses the same proposal tools and never receives secrets:
  values are masked as `***` before any prompt, as in API responses.
- **Hand editing stays complete.** Every proposable change is an ordinary
  form or API call. AI is never the only way to do something.

## Migration plan and risks

1. **Done in this PR.** The project sidebar, project pages and redirects:
   - `/p/:slug?tab=variables` → `/p/:slug/variables`
   - `/p/:slug?tab=settings&section=…` → `/p/:slug/settings?section=…`
   - other `?tab=` values → `/p/:slug`
   - unknown `/p/:slug/*` → `/p/:slug`
   - `/monitoring/providers/edit/:id` → `/settings/notifications/:id`

   Bookmarks keep working. The `?section=` and other parameters, and the
   `#hash`, ride along.
2. **Backend filters** (table above), then drop the console fan-out.
3. **Views:** the `views` table, the panel registry, then migrate
   `metric_dashboards` (the old URLs redirect).
4. **Persisted proposals** and the project timeline.

Risks:

- **Bookmarks and docs.** Documentation and screenshots that show the
  server-wide sidebar inside a project, or `?tab=` URLs, are out of date.
  The redirects cover links; docs need a pass.
- **Plugins.** `navItems` still land in the global sidebar only. A plugin page
  that is per project has no project entry until the panel/nav contract
  gains a project-scoped slot. Until then it stays reachable from the global
  sidebar and ⌘K.
- **Large projects.** The fan-out shows the first 25 services and says so,
  until the backend filters land.
- **Muscle memory.** Operators used to the global Databases or Domains list
  while inside a project now get the project's list. "All … on this server" is
  one click away.
- **Standalone services** keep their own menu. If most installs never group
  services, step 2's value depends on nudging grouping, which the overview
  already offers.

## Open questions

1. Should a standalone service be shown as a one-service project, so there is
   only one sidebar shape?
2. Project Deployments, Errors, Traces and Analytics need backend filters to
   show every service at once. Is `project_group_id` on each list the right
   shape, or should there be a generic `scope` parameter?
3. Should the overview's Databases section be dropped now that the project
   has a Databases page (see the duplicate-path table in the PR)?
4. Views: are they per user with sharing (proposed), or per project by
   default?
5. Which roles may approve an agent's proposal: the role the change needs, or
   a separate "approver" permission?
6. Is the built-in chat kept at all, given that operators prefer their own
   harness? If kept, is it limited to local models?
