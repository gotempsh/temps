# Console page inventory and a method for redesigning navigation

**Status:** Draft for review
**Date:** 2026-11-25
**Builds on:** ADR-112 (a project-focused console)

## Summary

This is every page the console routes to, where each one sits in a menu, and
where the same thing can be reached from more than one place. The last two
sections propose how to decide what stays prominent, using measured usage
rather than opinion.

Four findings shape the rest:

- **The console has seven menus and about 150 menu entries** for 198 page
  components. One service alone has 15 sidebar sections holding 58 tabs.
- **Most duplication is one concept split across scopes and names**, not one
  page listed twice. Alerts live in five places, metrics in seven, logs in
  seven.
- **"All tools" repeats 15 of the 21 global sidebar entries**, and is the only
  menu home for nine other pages.
- **There is no page-level usage data today.** Any ranking of pages by use is
  a guess until that is measured, so the tiers in this document are marked as
  hypotheses.

## How the console is organised today

The URL decides which sidebar is shown (`web/src/lib/console-scope.ts` and
`web/src/components/dashboard/Sidebar.tsx`).

| Menu | Shown on | Entries | Defined in |
|---|---|---|---|
| Global sidebar | everything outside a project | 21, in 4 groups, plus plugin entries | `web/src/components/dashboard/primary-navigation.ts` |
| All tools | `/tools`, a page of links | 26, in 4 groups | `web/src/components/platform/platform-tools.ts` |
| Settings sidebar | `/settings/*` | 25, in 4 groups | `web/src/components/settings/settings-navigation.ts` |
| AI sidebar | `/ai-gateway/*`, `/chat` | 5 | `web/src/components/dashboard/Sidebar.tsx` |
| Project sidebar | `/p/:slug/*` | 11, in 4 groups, plus the services list | `web/src/lib/console-scope.ts` |
| Service menu | `/projects/:slug/*` | 15 sections, 58 tabs | `web/src/lib/project-navigation.ts` |
| Node sidebar | `/settings/nodes/:id/*` | 5 | `web/src/lib/node-detail.ts` |

A project is an API `project_group`; a service is an API `project`. A service
inside a project keeps the project sidebar, with its own pages folded under
its name until the user opens them. A standalone service shows the service
menu directly.

Other ways in, on every page:

- **Header:** breadcrumbs, a service switcher, Drop, the AI assistant, the
  operations tray and backup alerts.
- **Command palette (⌘K):** Recent, Navigation, Settings, Observe, Plugins,
  Account and Actions. It indexes all tools, every settings page, individual
  settings and each project's pages.
- **Sidebar footer:** help links and the account menu.
- **Setup progress card** in the global sidebar, linking to `/setup`.

## Inventory

"Menu home" is where a menu lists the page. "None" means it is reached only
from a link on another page, the palette or a typed URL.

### Global scope

#### Applications

| Page | URL | Menu home | Pages below it |
|---|---|---|---|
| Projects | `/projects` (also `/` and `/dashboard`) | Sidebar, All tools | New service `/projects/new`, import wizard `/projects/import-wizard`, import a repository `/projects/import/:repositoryId` |
| Get started | `/get-started` | Sidebar | |
| Finish setting up | `/setup` | Setup progress card | |
| Connect AI harness | `/setup/ai` | All tools | |
| Drop | `/drop` | Header button | |
| Workflows | `/workflows` | Sidebar | Tabs: Workflows, Runs. Per environment: list, settings, grants, run detail, keys |
| Vault | `/vault` | Sidebar | Vault detail tabs: Services, Access bundles, Sessions, Linked to, Audit logs. Bundle and session pages, and a per-service variant under `/vault/projects/:id` |
| Access services | `/access-services/new`, `/access-services/:id/edit` | None | Reached from Vault |
| Git connections | `/git-providers` | Sidebar, All tools | Add, provider detail, connection detail |
| Domains | `/domains` | Sidebar, All tools | Add, domain detail, tunnel domain detail |
| Certificates | `/certificates` | All tools | |
| DNS providers | `/dns-providers` | All tools | Add, detail |
| Delivery profiles | `/delivery-profiles` | All tools | Detail |
| Email | `/email` | Sidebar, All tools | Tabs: Overview, SDK. Email detail, domains, providers |
| Worker nodes | `/settings/nodes` | Sidebar | Ingress setup. Node pages: Overview, Monitoring, Networking, Builds, Scheduling |
| Sandboxes | `/sandboxes` | All tools | Sandbox detail, settings (Runtime, Preview gateway) |
| Workspaces | `/workspaces`, `/workspaces/:id` | None | Reached from Sandboxes |

#### Storage

| Page | URL | Menu home | Pages below it |
|---|---|---|---|
| Databases | `/storage` | Sidebar, All tools | Tabs: Databases, Platform services. Create, import. Database pages: detail, monitoring, browse, logs, query performance, restore, import data, upgrade, add member |
| Backups | `/backups` | Sidebar, All tools | S3 source: new, detail, backup detail. Schedule: new, edit, detail, run detail |

#### Observability

| Page | URL | Menu home | Pages below it |
|---|---|---|---|
| Analytics | `/analytics` | Sidebar, All tools | |
| Revenue | `/revenue` | All tools | |
| Traces | `/traces` | Sidebar, All tools | Cross-project trace `/traces/global/:traceId` |
| Logs | `/logs` | Sidebar, All tools | |
| Errors | `/errors` | Sidebar, All tools | |
| Server metrics | `/monitoring/server` | Sidebar, All tools | |
| Monitoring | `/monitoring/alerts` | Sidebar, All tools | Sections: Alerts, Alert rules, Alarms, Delivery preferences |
| Proxy | `/proxy` | Sidebar, All tools | |
| Proxy logs | `/proxy-logs` | All tools | Log detail |
| Status pages | `/status-pages` | Sidebar, All tools | New, detail |
| Audit logs | `/audit-logs` | All tools | |
| IP details | `/ip/:ip` | None | Reached from any IP address shown in a list, such as proxy logs and Shield events |

#### Administration and AI

| Page | URL | Menu home | Pages below it |
|---|---|---|---|
| All tools | `/tools` | Sidebar | |
| Plugins | `/settings/plugins` | Sidebar | Tabs: Browse, Running. Install |
| Plugin pages | `/plugins/:pluginName/*` | Sidebar (one entry per plugin) | |
| Settings | `/settings` | Sidebar, All tools | See Settings below |
| Account | `/account` | Account menu | Account settings, Change password, Two-factor authentication |
| Temps assistant | `/chat` | Header button, All tools | |
| AI providers | `/ai-gateway` | All tools | AI sidebar: Providers, Usage, Models, Limits, Activity |
| Agent providers | `/agent-sandbox/providers`, `/agent-sandbox/providers/:id` | None | Reached from the assistant when a provider fails |
| CLI login | `/cli-login`, `/cli-login/:userCode` | None | Opened by the CLI |

### Settings (`/settings/*`)

| Group | Pages |
|---|---|
| General | Platform, Version, Notifications (tabs: Providers, Routes; new and edit forms), Temps Cloud, Telemetry |
| Access | Users (new, detail), Teams (detail), Authentication (new provider, provider detail), API keys (new, detail, edit) |
| Infrastructure | Load balancer (add route), Docker registry, Build limits, Request timeouts, Service health checks, DNS (tabs: Overview, Records, Queries, Test), Traefik discovery, MCP server |
| Security | Security, Shield proposals, On-demand TLS, Network policy, Rate limiting, Disk monitoring, Metrics monitoring, OTel pipeline |

Worker nodes and Plugins have `/settings/...` URLs but are listed in the
global sidebar, not here.

### Project (`/p/:slug/*`)

| Group | Page | URL below `/p/:slug` | Shows |
|---|---|---|---|
| | Overview | (root) | Services, routes, databases, last deploy |
| Services | one row per service | links to `/projects/:slug` | |
| Observe | Deployments | `deployments` | Every service's deployments |
| Observe | Logs | `logs` | Every service's logs |
| Observe | Errors | `errors` | One service at a time, picked on the page |
| Observe | Traces | `traces` | One service at a time |
| Observe | Analytics | `analytics` | One service at a time |
| Resources | Domains | `domains` | Domains of the project's services |
| Resources | Databases | `databases` | Linked databases, link and unlink, create |
| Resources | Backups | `backups` | Backups of the project's databases |
| Configure | Shared variables | `variables` | |
| Configure | Settings | `settings` | Project identity, Repository, Path routes, Delete project. Connect repository at `repository` |

A page is hidden from this menu when none of the project's services can have
it: a project of monitored services only has no Deployments, Domains,
Databases, Backups or Shared variables.

### Service (`/projects/:slug/*`)

| Group | Section | Tabs | Record pages below |
|---|---|---|---|
| | Overview | Overview, Activity | |
| Ship | Deployments | Deployments | Deployment detail, Drop |
| Ship | Environments | Environments (views: Containers, Metrics, Settings, Shield) | Container detail, Reachability |
| Ship | Domains | Domains | |
| Ship | Databases | Databases, Key-value store, Blob storage | |
| Observe | Logs | Application, Requests, Telemetry | Request detail |
| Observe | Errors | Issues, Alerts, Feedback, Releases, Source maps, Inbound filters, Attachments, Ownership, SDK health, Setup | Issue detail, event detail, autofix, alert rule form, source map credentials |
| Observe | Traces | Traces, AI traces | Trace detail, Operations |
| Observe | Analytics | Overview, Visitors, Live, Globe, Journey, Sessions, Pages, Funnels, Surveys, Activity report, Web performance, AI visitors, AI crawlers, API traffic, Revenue, Traffic filters | Visitor detail, session, session replay, event, dimension, segment, funnel forms, Setup |
| Observe | Monitoring | Metrics, Dashboards, Alerts, Uptime, Cron jobs, Check-ins, Durable workflows | Dashboard view, builder, alert form, monitor detail, cron detail |
| Configure | Build & deploy | Build & deploy (sections: Source, Repository, Build, Deployment, Previews, Feature flags, Host access) | Connect repository flow |
| Configure | Variables & secrets | Environment variables, Secrets, Deployment tokens | Variable detail, checks |
| Configure | Security & access | Protection, Edge rules, Access, Scans | Edge rule editor, scan detail, vulnerability detail |
| Configure | Automation & integrations | Automation, Integrations, Telemetry | Agent run detail, webhook forms and detail |
| Configure | General | General | |

Analytics (16 tabs) and Errors (10 tabs) hold 26 of the 58 tabs.

A monitored service keeps only the pages it can have (analytics, errors,
traces, telemetry logs, metrics, uptime, check-ins, general and telemetry
settings) and gets one extra entry, Add hosting (`hosting`).

Service pages with no menu entry:

| Page | URL below `/projects/:slug` | Reached from |
|---|---|---|
| Status pages | `status-pages`, `status-pages/:id` | Redirects to the global Status pages |
| Analytics setup | `analytics/setup` | The service overview's empty analytics cards |
| Trace operations | `traces/operations` | The traces list |
| Globe (second route) | `analytics/globe` | `analytics/live` is the listed tab |
| Request analytics | `analytics/requests/*` | Old links only; it redirects |

### Outside the console shell

Login, MFA verification, forgot password, reset password, required password
change, sandbox preview access and ingress login.

### Redirects kept for old links

`/settings/domains`, `/settings/email`, `/settings/ai-gateway`,
`/settings/ai-providers`, `/settings/agent-sandbox`, `/settings/skills`,
`/settings/mcp-servers`, `/settings/git-providers`, `/settings/dns-providers`,
`/settings/backups`, `/alarms`, `/monitoring/resources`, `/ai-first`,
`/ai-workflows`, `/skills`, `/mcp-servers`, `/agent-sandbox/*`, plus 15
service-level ones listed in `web/src/lib/project-settings-sections.ts`.

## Duplicates and overlaps

### The same entry in two menus

| What | Where |
|---|---|
| 14 global pages | Global sidebar and All tools: Projects, Git connections, Databases, Backups, Email, Domains, Server metrics, Analytics, Traces, Logs, Errors, Monitoring, Proxy, Status pages |
| Settings | Global sidebar (bottom) and All tools |
| MCP server | Settings › Infrastructure and All tools › Automate |
| Temps assistant | Header button and All tools |

Nine pages have All tools as their only menu home: Sandboxes, Certificates,
DNS providers, Delivery profiles, Revenue, Proxy logs, Audit logs, Connect AI
harness and AI providers. So All tools is both a repeat of the sidebar and
the sole route to pages that have nothing else in common.

### One concept in several places

| Concept | Places | Count |
|---|---|---|
| Metrics | Server metrics; service Monitoring › Metrics; service Environments › Metrics view; service Overview CPU and memory; node Monitoring; database Monitoring; Settings › Metrics monitoring | 7 |
| Logs | Global Logs; project Logs; service Logs (3 tabs); Proxy logs; database Logs; Audit logs; deployment build logs | 7 |
| Alerts | Global Monitoring (Alerts, Alert rules, Alarms); service Errors › Alerts; service Monitoring › Alerts; node alert rules; header backup alerts | 5 |
| Domains and TLS | Global Domains; project Domains; service Domains; Certificates; DNS providers; Delivery profiles; Settings › DNS; Settings › On-demand TLS; Settings › Load balancer | 9 |
| Databases | Global Databases; project Databases; project Overview's database list; service Databases (3 tabs) | 4 |
| Backups | Global Backups; project Backups; database Restore; header backup alerts | 4 |
| Variables and secrets | Project Shared variables; service Environment variables; service Secrets; Vault; service Integrations › vaults | 5 |
| Analytics | Global Analytics; project Analytics; service Analytics (16 tabs); service Overview cards; global Revenue and service Revenue | 5 |
| Notifications | Settings › Notifications (providers, routes); Monitoring › Delivery preferences | 2 |
| Repository | Git connections; service Build & deploy › Repository; project Settings › Repository | 3 |
| Keys and tokens | Settings › API keys; service Deployment tokens; workflow keys; AI provider keys | 4 |
| Scheduled and background work | Global Workflows; service Monitoring › Durable workflows; Cron jobs; Check-ins; service Automation | 5 |
| Onboarding | Get started; Finish setting up; Connect AI harness; New service; Import wizard; Drop | 6 |
| AI | Temps assistant; AI providers (5 pages); Connect AI harness; Settings › MCP server; service Traces › AI traces; service Automation; Analytics › AI visitors and AI crawlers | 8 |

Some of these are correct: a project's Databases page and the server's
Databases page answer different questions. The cost is that each scope names
and groups the concept differently, so knowing where metrics are in one
place does not tell the user where they are in another.

### One name for different things

| Name | Means |
|---|---|
| Telemetry | Settings › Telemetry (what this instance reports to its maintainers); service Logs › Telemetry (OpenTelemetry logs); service Automation & integrations › Telemetry (ingest settings) |
| Monitoring | A global page of alerts; a service section of seven tabs; a node page; a database page; two settings pages |
| Security | A settings group, a settings page inside that group, and a service section |
| Overview | The service's first tab, and the first tab of its Analytics |
| Activity | Service Overview › Activity, Analytics › Activity report, AI providers › Activity |
| Alerts | Three separate rule lists with three forms |

### One thing with different icons, or one icon for two things

| Concept | Icons used |
|---|---|
| Errors | Shield with alert (global sidebar), bug (All tools), octagon (project and service) |
| Traces | Fork (sidebars), workflow (All tools) |
| Vault and Errors | The same shield icon, in the same global sidebar |
| Workflows and Traces | The same workflow icon, in the sidebar and All tools |

### URL and menu disagree

- Worker nodes and Plugins are at `/settings/...` but listed in the global
  sidebar, which needs special cases to keep the sidebar from swapping.
- Server metrics is at `/monitoring/server` but is a separate sidebar entry
  from Monitoring.
- Service AI traces is at `ai-gateway`, the same word as the global AI
  providers area.
- Service Databases is at `storage`; project Databases is at `databases`.

## What is known about usage

Nothing at the page level. What exists:

| Source | Records | Use for this work |
|---|---|---|
| Product telemetry (`crates/temps-product-telemetry`) | Three first-time events: project created, Git provider connected, deploy attempted | Onboarding funnel only |
| Command palette frecency | Each browser's recent destinations, in local storage | Never leaves the browser |
| Audit logs | Every write, by kind | Shows what people change and how often, not what they read |

So "based on usage" needs a measurement step first. Audit logs can give a
first ranking of write actions today, with no new code.

## Method

The unit is a task, not a page. Pages follow from tasks.

### 1. Write the task list

About 30 tasks in the user's words ("see why the last deploy failed", "add a
domain to a service"), each mapped to the pages it needs today and the number
of clicks from a cold start at `/projects`.

### 2. Measure three things per task

| Measure | Question | Source |
|---|---|---|
| Reach | What share of weekly users do this in a week? | Page-view counts |
| Frequency | How often does one user do it in a week? | Page-view counts |
| Criticality | What does it cost when the user cannot find it? | Judged, 1 to 3. Restore a backup is rare and a 3 |

### 3. Collect the data

- **A page-view counter in the console.** It records the route pattern
  (`/projects/:slug/errors/:id`, never the slug or ID), how the user arrived
  (sidebar, tab, palette, link on a page, typed URL) and the scope. About 300
  patterns, so cardinality is bounded. Counts are aggregated in the browser,
  sent in batches, stored on the instance and shown to its administrator. They
  leave the instance only if product telemetry is on. This is new backend
  work.
- **Palette searches**, including those with no result. A search for a page
  that has a sidebar entry means the entry was not found.
- **A top-tasks survey.** Each respondent picks 5 of the 30 tasks. This covers
  instances that share no telemetry.
- **A first-click test** on any proposed menu before building it: show the
  menu, name a task, record the first click. A task passes at 80% correct
  first clicks.

### 4. Apply fixed rules

Decide the rules before reading the numbers, so the data settles arguments.

| Tier | Rule | Where it lives |
|---|---|---|
| Everyday | The smallest set of pages covering 80% of page views in a scope | A sidebar entry, at most 7 per scope |
| Occasional | Remaining pages with weekly reach above a threshold set from the data | A tab under an everyday page, one click away |
| Rare | Everything else | A tab or a link on the everyday page it relates to, and the palette |
| Rare and critical | Criticality 3, whatever its reach | One click from the related everyday page, never palette-only |

Three rules hold whatever the data says:

- **One home per page.** A page is listed in exactly one menu. Other places
  link to it; they do not list it again.
- **One name and one icon per concept**, in every scope.
- **No catch-all entries.** A menu entry names a job or a thing ("Domains",
  "Deployments"), never a container for whatever did not fit ("All tools",
  "More", "Other", "Advanced"). A page that fits nowhere is a sign the page
  or the grouping is wrong.

### 5. Check the result

After a change, compare against the baseline:

- median clicks for each of the top 10 tasks;
- share of visits to everyday pages that come through the palette (should
  fall);
- palette searches with no result (should fall);
- no page losing all its views without a decision to retire it.

## First cut, as hypotheses

These tiers come from reading the code and from the direction in ADR-112, not
from data. Step 3 confirms or changes them.

| Tier | Task | Today |
|---|---|---|
| Everyday | See whether the last deploy worked, and why not | Deployments, deployment detail |
| Everyday | Read a running service's logs | Logs › Application |
| Everyday | See new errors | Errors › Issues |
| Everyday | Change a variable or a secret | Variables & secrets |
| Everyday | Open the app and check it is healthy | Overview |
| Everyday | Redeploy or roll back | Deployments |
| Occasional | Check traffic | Analytics › Overview |
| Occasional | Look at or query a database | Databases › Browse |
| Occasional | Add a service, a domain or a database | Three different flows |
| Occasional | Check CPU and memory | Four places, depending on scope |
| Occasional | Follow a slow request | Traces |
| Rare | Connect Git, DNS, email or notification providers | Global sidebar, All tools and Settings |
| Rare | Users, teams, authentication, API keys | Settings › Access |
| Rare | Worker nodes, plugins, platform limits | Global sidebar and Settings |
| Rare and critical | Restore a backup | Database menu, or Backups |
| Rare and critical | Block abusive traffic | Service Security & access › Edge rules, or Settings › Rate limiting |
| Rare and critical | Rotate a leaked secret | Secrets, Vault, API keys or Deployment tokens, depending on the kind |

If this holds, six everyday tasks need five pages, against 58 tabs in the
service menu.

### Changes that need no usage data

These remove duplication without moving anything a user depends on:

1. Remove All tools (decided). Its name says nothing about what is behind
   it, and it exists only because nine pages had no other home. Each gets
   one, listed below; `/tools` redirects to Projects.
2. Give each concept one name and one icon across scopes: Errors, Traces,
   Vault, Telemetry, Monitoring.
3. Put the three alert-rule lists behind one Alerts page per scope.
4. Remove or link the service pages that have no menu entry.

Where the nine pages go:

| Page | New home | Why there |
|---|---|---|
| Certificates | A tab of Domains | A certificate belongs to a domain |
| DNS providers | A tab of Domains | They exist to verify and issue for domains |
| Delivery profiles | A tab of Domains | They choose how a domain is served |
| Proxy logs | A tab of Proxy | The requests behind the Proxy page's charts |
| Revenue | A tab of Analytics | Already a tab of a service's Analytics |
| Audit logs | Settings › Access | Who did what, next to who may do what |
| Sandboxes | Its own sidebar entry, under Applications | A thing users run, like a project |
| AI providers | An AI sidebar entry, opening the menu it already has | Five pages with no way in today |
| Connect AI harness | The first page of that AI menu, merged with Settings › MCP server | One "Connect an agent" page instead of two |

The global sidebar goes from 21 entries to 22: All tools leaves, Sandboxes
and AI arrive. Five pages become tabs of a page that is already there, and
Audit logs moves into Settings.
The command palette keeps indexing every page, so it stays the way to jump
to anything by name.

## Open questions

1. Is the page-view counter acceptable as described: on by default, stored
   on the instance, shared only with product telemetry on?
2. Whose usage sets the tiers when instances differ: a solo operator with
   three services, or a team with a hundred?
3. Should a standalone service become a one-service project, so the console
   has one sidebar shape instead of two? (ADR-112, open question 1.)
4. Is seven sidebar entries per scope the right cap?
