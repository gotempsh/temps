# Console navigation: a proposal

**Status:** Revision 4 (one row of tabs per page; a service's views in the sidebar) is built on top of revision 3's tree. Revisions 1 to 3 below are kept as history
**Date:** 2026-11-25
**Builds on:** [Console page inventory](./console-page-inventory.md), ADR-112

## Revision 4, built: one row of tabs, a service's views in the sidebar

Revision 3 put an entity's views in a strip above its page and the pages of
the view in a second strip under it, sometimes a third. That was two
problems: on a phone each row was cut off at the screen edge, and a settings
page sat in a narrower, re-centred column, so the strips moved and shrank
when going from Deployments to Settings.

What is built now, replacing the statements below wherever they differ:

- **A page has one row of tabs at most.** Stacked strips are forbidden in
  the design system (`DESIGN.md` section 1, `web/packages/ds/docs/RULES.md`),
  and the tab strip component has no levels to ask for one.
- **A service's views are rows under the service in the tree**, at every
  width: Overview, Deployments, Logs, Errors, the data it has, Settings,
  then "Add" for views that hold nothing yet. Only the service shown is
  opened. On a phone the tree is the drawer, and a button on the page names
  the view shown and opens it. Folded to icons, the sidebar shows one icon
  per view.
- **The one row left is the pages of the view shown** (Settings: General,
  Data ingest, Protection, Edge rules, Access, Scans, Automation,
  Integrations). A page that holds several (Errors > Settings, Analytics >
  Visitors, Pages, Funnels, Settings) is a tab that opens a menu and says
  which page is shown.
- **Tabs that do not fit are listed under "More"**; the tab shown is never
  one of them. No strip scrolls sideways or wraps.
- **A project and the Projects page keep their views as the one row**; a
  view with several pages (a project's Databases, the server's Alerts and
  Analytics) is a menu tab there too.
- **Every page of a service is in one column.** Settings no longer narrow
  and re-centre the page: the tabs stay put, and only the form's body is
  kept to the form measure, from the same left edge.

Open: whether a project's views should move under the project in the tree
too. They share that row with the project's services, so it was left as the
one row of tabs.

## Revision 3: an entity tree

It replaces the parents (Observe, Resources, AI, Configure), their overview
pages and the three sidebars that swapped by level. Everything from "What
changed in revision 1" down describes what it replaced.

### Why

Revision 2 still had too much navigation. Reaching one page could take four
steps: a parent, an area, a tab, then a second row of views. The parents
grouped pages by kind of data (Observe, Resources), but nobody arrives
thinking "I want to observe". They arrive thinking about a thing they own:
a project, a service, a database, a domain, a server.

### The sidebar

One sidebar on every page: a tree of what the user owns. It opens to where
the user is and never swaps, except for Settings.

```
Find…

▾ Projects
   ▾ Storefront
        storefront-web        ●
        storefront-api        Down
        storefront-landing    ○
   ▸ Pipeline                 2 down
     standalone-service       ●
▸ Databases
▸ Domains
▸ Servers

Settings
```

- **A row carries its state.** Healthy is a quiet dot, idle a hollow one,
  and trouble is a word (Down, Degraded, Failed, Pending, Offline). A folded
  project says the worst of its services, counted ("2 down"). A service
  nothing checks has no mark: it is not called healthy.
- **No Logs, Errors, Traces, Analytics or Alerts entries.** They are views of
  an entity, so they are tabs of its page.
- **A folded branch asks the server nothing.** Projects starts open; the
  others open when the user is inside them or unfolds them, and the choice is
  remembered. With fewer than six rows under Projects, each project starts
  open too.
- **Long lists are cut** (8 rows per branch, 20 projects), with a row that
  leads to the full list. The row being shown is always kept.
- **An empty branch offers the one thing that fills it** (Add database, Add
  domain). A branch that failed to load says so and offers Retry.

### An entity's views are the tabs of its page

| Entity | Tabs | Second row |
|---|---|---|
| Projects (every project) | Projects, Logs, Errors, Traces, Analytics, Alerts | The pages of the tab shown (Alerts: Alarms, Rules, Events, Channels, Preferences) |
| A project | Overview, Deployments, Logs, Errors, then Traces, Analytics, Domains, Databases once in use, Shared variables, Settings | Databases: Databases, Backups |
| A service | Overview, Deployments, Logs, Errors, Variables, Traces\*, Metrics, Uptime\*, Analytics\*, Alerts, Domains\*, Databases\*, Settings (\* once in use) | The pages of the tab shown |
| Databases, Domains, Servers | Their own pages, as before (Databases and Backups; Domains, Certificates, DNS providers, Delivery; Nodes and This server) | |
| A server | Overview, Monitoring, Networking, Builds, Scheduling | |

- **A view that holds nothing is not a tab.** It is listed under "Add" at
  the end of the strip, with what is missing ("No trace received yet."), and
  one click opens its set-up page. A view whose answer is unknown (loading,
  failed, forbidden) stays a tab, so nothing is hidden on a guess.
- **A view none of a project's services can have is not offered at all**
  (Databases for a project of static sites), as before.
- **Settings is one tab of a service.** Security, Automation and the
  service's own settings are its second row, eight pages in one line.

### What moved where

| Before | Now |
|---|---|
| Observe, Resources, Configure, AI parents and their overviews | Removed |
| Server Logs, Errors, Traces, Analytics, Alerts | Tabs of the Projects page |
| Git, Email, Status pages, Workflows, Proxy | Settings › Platform |
| Model providers, Connect an agent, Sandboxes, Vault | Settings › AI |
| Assistant | The header button, as before |
| A project's and a service's own sidebars | Removed: the tree marks the project or service, its tabs carry its views |
| A server's own sidebar | Removed: its sections are tabs of its page |

URLs did not change. The parents' own URLs (`/observe`, `/resources`, `/ai`
and the same sub-paths of a project and a service) were never released and
are gone.

### Where the build differs from what was proposed

| Proposed | Built |
|---|---|
| An entity's page reads State, Needs attention, Data, Add, Settings | Only the tabs and "Add" are built. Each page's content is as it was; no "Needs attention" block exists |
| Alerts become "Needs attention" on the entity that is firing | Alerts is still a tab, of Projects and of a service |
| Depth is two steps at most | Three rows on a few service pages: Errors › Settings and Analytics › Visitors, Pages, Funnels, Settings each hold several pages |
| Settings is one link, top right | Settings is the last tab |
| The numbers panels of the Observe overview become "Needs attention" | Removed with the overview. Nothing replaces them yet |
| A database is listed under its project and under Databases | Under Databases only. A project's databases are its Databases tab |

### Open

1. **Sandboxes** are in Settings › AI. They also run, like services: should
   they be a fifth branch of the tree?
2. **Status pages, Workflows and Proxy** are in Settings › Platform though
   they hold data, not only configuration. Is Settings their right home?
3. **A service in no project** is listed beside the projects. Should it
   become a one-service project so the tree has one shape?
4. **"Needs attention"** on Projects and on a project: build it next, from
   what is firing and what failed to deploy?

## History: revisions 1 and 2 (replaced by revision 3)

Everything below describes the parent-based menus that revision 3 removed.
It is kept to show what was tried.

### What changed in revision 1

- **Every parent is a page.** Observe, Resources, AI and Configure were
  labels that only folded. Each is now a link to an overview of what it
  holds, what is active and what is not, with a button to activate each area
  that is not.
- **An area that is not active has no sidebar entry yet.** It lives on its
  parent's overview until it is activated. The sidebar shows what a team
  uses; the overview shows what it could use.
- **AI assistance is offered, not tucked away.** The group is renamed AI, the
  assistant is its first entry, and the header button stays on every page.
  The assistance itself is designed in separate work; this plan only fixes
  where it lives in the menu.

## What was built

Revision 2 records the console as it now stands. The menus, the parent
overviews and the settings regrouping are built at all three levels. Where
the build stops short of the proposal, the proposal's text below is kept and
the difference is listed here.

| Proposed | Built |
|---|---|
| An inactive area has no sidebar entry | Built. An area whose state is loading, failed or forbidden stays listed, so a page never goes missing because a question went unanswered. The last known state is remembered per browser |
| Every area reports whether it is active | Built for the areas in the table under "What \"active\" means". Server Traces, Analytics, Alerts, Connect and Vault, and service Alerts, have no state yet: they are always listed and their card has no set-up button |
| Activation in one step on the card | Not built. Every set-up button opens the area's own page, which carries its set-up state |
| Errors opens a set-up page until its first event | Not built. Errors opens its list, with that page's own empty state |
| Errors and Analytics: fewer tabs, with merged pages | Built as a second row of views under a tab (Errors › Settings, Analytics › Visitors, Pages, Funnels, Settings). The pages themselves are unchanged |
| Service Alerts: one list | Two tabs, Error alerts and Metric alerts. The two lists and their forms are unchanged |
| Activity becomes a section of the service Overview | It is a tab of Overview, at `activity`. The old `observe` URL now opens the Observe overview |
| Server tabs: Ingress, Platform services, Sandboxes › Workspaces and Settings | Not built. No such pages exist to place |
| AI › Connect is one page | Two tabs, Agent and MCP server |
| Settings overview shows what needs attention | It lists the groups and their pages, with one line each. No attention items |
| A standalone service becomes a one-service project | Not built. A standalone service keeps a menu of the same shape |

## Summary

One console, with one menu shape at every level: a home page, a few everyday
entries, then three or four parents. A parent is a page of its own and opens
to show the areas a team has activated.

| | Today | Proposed |
|---|---|---|
| Menus | 7, each grouped its own way | 5: server, project, service, node, settings |
| Server sidebar | 21 entries, plus 9 pages reachable only from All tools | 3 entries and 4 parents; up to 16 areas under them, shown once active |
| Service menu | 15 sections always open, 58 tabs | 5 entries and 3 parents; up to 10 areas under them, about 40 tabs |
| Group labels | Not clickable | Each is an overview page |
| A feature nobody set up | A sidebar entry that opens an empty page | A card on its parent's overview with one button |

Nothing is removed. Every page in the inventory has one place below.

## Rules

1. **One home per page.** Other pages link to it; no second menu lists it.
2. **One name, one icon, one order** for a concept at every level. Logs is
   called Logs, has the same icon and sits in the same position on the
   server, in a project and in a service.
3. **No catch-all entries.** No "All tools", "More", "Other" or "Advanced".
4. **Every parent is a page.** Clicking a parent's name opens its overview;
   the chevron beside it folds and unfolds its areas. This is the pattern a
   service row already uses inside a project.
5. **The sidebar lists active areas; the parent's overview lists all of
   them.** An area is never hidden: until it is active it is one click away,
   on the overview, with the button that activates it.
6. **Everyday entries sit above the parents**, always visible. The everyday
   set starts as the guess below and is replaced by measured usage.
7. **Tabs are for things to look at. Set-up lives behind the area's
   Settings.** Source maps or traffic filters are configured once; they do
   not sit beside the pages opened every day.

## Parent overviews

A parent's overview is an index with state, not a dashboard. One card per
area:

| Area state | The card shows | Button |
|---|---|---|
| Active | Name, one current number, a link | Open |
| Not active | Name, one line on what it does, what is missing | One verb that activates it: "Set up errors", "Add monitor", "Connect Git" |
| Not available here | Name and why (a monitored service has no deployments) | None |

The button does the job where it is, in one step where that is possible, and
the card turns active in place. Longer set-up (an SDK to install) opens the
area's own set-up page, which is also what a direct link to an inactive area
shows.

The home page answers "is it working". A parent's overview answers "what is
in here, and what is switched on". They do not repeat each other: the home
page has no index, and an overview has no charts.

### What "active" means

Proposed, per area. The server already answers `configured` and `setup_path`
for several of these; the others need the same answer added.

| Area | Active when | Button when not |
|---|---|---|
| Traces (project, service) | The first span has arrived | Set up tracing |
| Analytics (project, service) | The first page view has arrived | Add analytics |
| Uptime | A monitor exists | Add monitor |
| Status pages | One exists | Create status page |
| Databases | One exists (server), or is linked (project, service) | Add database |
| Domains | A custom domain exists | Add domain |
| Email | A provider exists | Add provider |
| Workflows | A workflow has registered | Set up workflows |
| Git | A connection exists | Connect Git |
| Assistant, Providers | A model provider is configured | Add model key, Add provider |
| Sandboxes | A sandbox exists | Create sandbox |

In a project an area is active as soon as one service uses it, and its card
says how many do ("2 of 4 services").

Not built yet, as proposed in revision 1:

| Area | Active when | Button when not |
|---|---|---|
| Errors | The first event has arrived | Set up errors |
| Alerts | A rule and a channel exist | Add alert |
| Backups (a tab of Databases) | Backup storage and a schedule exist | Add backup storage |
| Traces, Analytics (server) | Any service sends them | Set up tracing, Add analytics |
| Connect an agent | A key has been issued to an agent | Connect agent |
| Vault | A vault exists | Create vault |

Always active, with no card state: Deployments, Logs, Variables, Metrics,
Servers, Proxy, Security, Automation, Settings.

## The same shape at every level

| Position | Server | Project | Service |
|---|---|---|---|
| Home | Projects | Overview | Overview |
| Everyday | Logs, Errors | Deployments, Logs, Errors | Deployments, Logs, Errors, Variables |
| Observe | Traces, Analytics, Alerts, Status pages | Traces, Analytics | Traces, Metrics, Uptime, Analytics, Alerts |
| Resources | Servers, Databases, Domains, Proxy, Email, Workflows, Git | Domains, Databases | Domains, Databases |
| AI | Assistant, Connect, Sandboxes, Vault, Providers | | |
| Configure | Settings | Shared variables, Settings | Security, Automation, Settings |

Observe, Resources, AI and Configure are the parents. A user who learns
where Logs is once knows where it is everywhere.

## Server

Shown outside a project. `›` marks a parent, which is a page.

```
Projects

Logs
Errors

Observe ›
  Traces
  Analytics          tabs: Analytics, Revenue
  Alerts             tabs: Alarms, Rules, Events, Channels, Preferences
  Status pages

Resources ›
  Servers            tabs: Nodes, This server
  Databases          tabs: Databases, Backups
  Domains            tabs: Domains, Certificates, DNS providers, Delivery
  Proxy              tabs: Traffic, Requests
  Email
  Workflows
  Git

AI ›
  Assistant
  Connect            tabs: Agent, MCP server
  Sandboxes
  Vault
  Providers          tabs: Providers, Usage, Models, Limits, Activity

Settings ›
```

On a new install the sidebar is Projects, Logs, Errors and the four parents.
An open parent lists only the areas that are set up or have no state to
report: Traces, Analytics and Alerts under Observe, Servers and Proxy under
Resources, Connect and Vault under AI. Everything else is a card on a
parent's overview until it is set up.

| Change | From |
|---|---|
| All tools is removed | Its nine pages are placed above and in Settings. The parents' overviews do its indexing job, each under a name that says what is in it |
| Alerts | Was "Monitoring". Channels was Settings › Notifications; Preferences was Monitoring › Notifications. The two are tabs side by side, not yet one page |
| Servers | Was "Worker nodes". "This server" was the separate Server metrics entry |
| Backups | Was a sidebar entry; now a tab of Databases, with Restore on each database |
| Certificates, DNS providers, Delivery profiles | Were only in All tools |
| Proxy › Requests | Was Proxy logs, only in All tools |
| AI › Connect | Holds Connect AI harness and Settings › MCP server, as two tabs |
| AI › Providers | Was a separate five-entry sidebar; now tabs |
| Get started | Not an entry. The parents' overviews carry the set-up buttons; the setup card shows progress until it is done, and search still opens the page |
| Plugins | Moves to Settings › Extensions. Pages a plugin adds keep their own entries |

## Project

```
Overview
Services             one row per service; its menu opens under its name

Deployments
Logs
Errors

Observe ›
  Traces
  Analytics

Resources ›
  Domains
  Databases          tabs: Databases, Backups

Configure ›
  Shared variables
  Settings
```

A project's overviews cover all its services: Observe says how many
services send traces and analytics, with the button to set up an area none
of them uses. Naming the services that do not is not built.

## Service

```
Overview             tabs: Overview, Activity

Deployments          tabs: Deployments, Environments, Build
Logs                 tabs: Application, Requests, OpenTelemetry
Errors               tabs: Issues, Feedback, Releases           + Settings
Variables            tabs: Variables, Secrets, Deploy tokens

Observe ›
  Traces             tabs: Traces, AI traces
  Metrics            tabs: Metrics, Dashboards
  Uptime             tabs: Monitors, Cron jobs, Check-ins, Workflows
  Analytics          tabs: Overview, Visitors, Pages, Funnels, Revenue   + Settings
  Alerts             tabs: Error alerts, Metric alerts

Resources ›
  Domains
  Databases          tabs: Databases, Key-value, Blob

Configure ›
  Security           tabs: Protection, Edge rules, Access, Scans
  Automation         tabs: Automation, Integrations
  Settings           tabs: General, Data ingest
```

`+ Settings` is a tab whose pages sit in a second row under it. Analytics
also has a second row under Visitors, Pages and Funnels.

Errors is an everyday entry and also needs set-up. Until its first event
arrives, the entry should open the set-up page, not an empty list (not
built).

| Change | From |
|---|---|
| Deployments holds Environments and Build | Three sidebar entries: Deployments, Environments, Build & deploy |
| Errors: 3 tabs | 10 tabs. Source maps, Inbound filters, Attachments, Ownership, SDK health and Setup move to Errors › Settings |
| Analytics: 5 tabs | 16 tabs. Live, Globe, Journey and Sessions become views of Visitors; Web performance and API traffic become views of Pages; Surveys joins Funnels; AI visitors and AI crawlers join Visitors; Traffic filters and Activity report move to Analytics › Settings |
| Metrics and Uptime | Were one "Monitoring" section of seven tabs. Environments › Metrics links to Metrics |
| Alerts | Was Errors › Alerts and Monitoring › Alerts. Now one area with those two lists as tabs |
| OpenTelemetry (a Logs tab) | Was "Telemetry" |
| Data ingest (a Settings tab) | Was "Telemetry", under Automation & integrations |

## AI assistance

The assistance itself (what it can do, where it appears inside pages, which
model it uses) is designed in separate work. This plan gives it three fixed
places, so that work has somewhere to land:

| Place | What it is |
|---|---|
| The header button, on every page at every level | Opens the assistant. Before a model provider exists it opens the assistant's own set-up page, so the button is never hidden and never dead. This work did not change the button |
| The AI parent, on the server | Its overview is the activation page: the assistant, connecting an outside agent, sandboxes, the vault and providers, each with its state and button |
| Inside pages | Wherever that work puts an action such as explaining an error or a failed deployment. Those actions are part of the page they act on, not menu entries |

A service's Automation (agents that act on that service, crash autofix)
stays under its Configure parent, since it is configured per service.

To confirm with that work: the name of the parent (AI), whether the
assistant needs a page of its own or only the header panel, and whether a
project or a service gets an AI entry too.

## Settings

Settings is a parent like the others: its overview shows each group and
its pages. Showing what needs attention (no sign-in provider, an update
available) is not built.

| Group | Entries |
|---|---|
| Access | Users, Teams, Sign-in (was Authentication), API keys, Audit log (from All tools) |
| Security | Security, Shield proposals, Network policy, Rate limiting, On-demand TLS |
| Instance | Instance (was Platform), Version, Load balancer, Docker registry, DNS, Traefik discovery, Limits (Build limits, Request timeouts and Service health checks as tabs) |
| Data | Metrics collection (was Metrics monitoring), Disk monitoring, OTel pipeline, Usage reporting (was Telemetry), Temps Cloud, Extensions (was Plugins) |

Notifications leaves for Alerts › Channels; MCP server leaves for AI ›
Connect.

## What this does not decide

- **Which entries are everyday.** The set above is a guess. The page-view
  counter in the inventory document replaces it with numbers.
- **URLs of existing pages.** They keep their URLs; only menus change. Each
  parent overview is a new URL. Two exceptions: a service's Activity moved
  from `observe` to `activity`, and `/tools` redirects to Projects.
- **Page content.** Merging tabs into views (Analytics, Errors) changes those
  pages, and each merge needs its own design.
- **The assistance itself**, as above.

## Order of work

1. **Server sidebar.** Remove All tools, place its nine pages, rename
   Monitoring and Worker nodes. Menus and redirects only. *Built.*
2. **Parents as pages**, at the server level first: the four overviews, with
   state taken from the answers the server already gives. *Built.*
3. **Active areas only in the sidebar**, once every area in step 2 reports
   its state reliably. Until then every area keeps its entry. *Built for the
   areas that report a state.*
4. **Project and service parents**, the same way. *Built.*
5. **Service menu regrouping:** Deployments, Metrics, Uptime, Alerts.
   *Built; Alerts is two tabs.*
6. **Errors and Analytics tab reductions**, one page at a time. *Built as a
   second row of views; no page was merged.*
7. **Settings regrouping.** *Built.*

Step 3 is the one that can make a page look missing, so it follows step 2
and not the other way round.

What remains, in the order that removes the most confusion first:

1. A state for the areas that have none (Errors, Alerts, server Traces and
   Analytics, Connect, Vault), each needing an answer from the server.
2. Activation on the card, in one step, where an area allows it.
3. One Alerts list per service, and one Channels page on the server.
4. Merged Errors and Analytics pages, each with its own design.
5. Attention items on the Settings overview.

## Open questions

1. Should an area that is not active have no sidebar entry (built this
   way), or a dimmed entry that opens its set-up page?
2. Once activated, can a team switch an area off again from the overview, so
   the entry goes away?
3. Should Alerts be one list per level (server, service), or is a single
   server-wide Alerts page with a filter enough?
4. Does a standalone service become a one-service project, so it needs no
   menu of its own?
