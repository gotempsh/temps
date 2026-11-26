# The sidebar, from first principles

**Status:** Steps 1 to 4 built; pins (step 5) not built
**Date:** 2026-11-26
**Builds on:** [Console navigation](./console-navigation-proposal.md) (revision 4, built), [Console page inventory](./console-page-inventory.md) (tiers and rules)

## Why the current tree feels wrong

Open a service today and the sidebar lists up to thirteen rows under it:

```
▾ Data Pipeline
   ▾ pipeline-ingest        ●
        Overview
        Deployments
        Logs
        Errors
        Variables
        Metrics
        Uptime
        Alerts
        Domains             ← a thing it owns
        Databases           ← a thing it owns
        Settings
        + Add
     pipeline-dashboard     ●
▸ Partner Sites
▸ Databases                 ← every database on the server
▸ Domains                   ← every domain on the server
▸ Servers
```

Three things are wrong with it, and they share one cause:

1. **Databases and Domains come last** because the views were ordered by
   category: what the service is, the data it has, what it owns, then how it
   is configured. Data came before ownership, so the things the service owns
   fell to the bottom, under Alerts. Order by category is not order by
   importance.
2. **Things and data are mixed in one list.** Domains and Databases are
   things you own, with a state of their own (a certificate failing, a
   database down). Logs and Metrics are data about a thing. The list reads
   them as equals.
3. **Too much at the top, too much inside.** Four branches at the top level,
   two of which (Databases, Domains) list the same records a project owns, and
   thirteen rows under one service, against the rule of at most seven
   everyday entries per scope.

## Principles

From what we agreed on 25 November ("show the most important entities at the
top and then show the data as it's needed"; make the 20% that yields 80% easy
and keep the other 80% possible) and the rules in the page inventory:

1. **The tree holds things you own; data is shown on a thing's page.** A row
   in the tree is something with a name and a state: a project, a service, a
   database, a domain, a server. Logs, errors, metrics and traces are never
   rows of their own.
2. **Very few rows at the top; the rest is found by opening.** The top level
   is what a user comes to work on: their projects. Everything else sits
   inside the thing it belongs to, and appears there only once it exists.
3. **Everyday first, at most seven per scope.** The everyday tasks of the
   inventory (did the deploy work, read logs, see new errors, change a
   variable, is it healthy, redeploy) are rows. Occasional pages are tabs of
   the everyday page they belong to. Rare pages are a tab or a link where they
   relate, and are in ⌘K.
4. **One home per page, no catch-alls.** Every page is listed once. No "More",
   "Other" or "All tools" row in the tree.
5. **It works with one project and with a hundred.** The tree never grows
   with the size of the server, only with what the user is looking at.

## Proposed tree

### Top level

```
Ask
Find…                         ⌘K

Pinned                               (only once something is pinned)
   storefront-api · Logs
   orders-db                  ●
   Checkout errors                   (a saved log search)

Projects
 ▸ Storefront                 1 down
 ▸ Data Pipeline
 ▸ Partner Sites
   standalone-service         ●
   + New project

Server                        1 offline
Settings
```

- **Projects is the only branch.** Databases, Domains and Servers stop being
  top-level branches: a database and a domain are found inside the project
  that uses them, and the server-wide lists move to the Server page.
- **Server is one row,** not a branch. It opens the server's page, whose tabs
  are Nodes, Databases, Domains, Backups (every record on the server, for the
  admin who needs the whole list). The row carries the server's worst state
  ("1 offline"), so trouble is visible without opening it.
- **A database or a domain that belongs to no project** is listed only on the
  Server page, which says so ("2 databases not used by any project").

### A project, opened

```
▾ Storefront
     storefront-web           ●
     storefront-api           Down
     storefront-landing       ○
     orders-db                ●      ⛁
     sessions-cache           ●      ⛁
     + Add
```

- **What it is made of, nothing else:** its services, then the databases its
  services use. Each is a row with its own icon and state.
- **"+ Add"** offers what a project can hold: a service, a database. It is
  the one place to add either from the tree.
- The project's own page keeps its views as tabs (Overview, Deployments,
  Logs, Errors, ...), as today.

### A service, opened

```
▾ storefront-web              ●
     Overview
     Deployments
     Logs
     Errors
     Variables
     Analytics                       (once it receives page views)
     Settings
     + Add                           (a view not in use yet: Analytics)
     Domains                  1 pending
```

- **Seven rows at most,** the everyday tasks, in the order they are done:
  is it healthy, did the deploy work, logs, errors, variables. Analytics is a
  row only once the service receives page views (for a website it is
  everyday; for an API it never applies). Settings is last.
- **Its custom domains are one Domains row,** after its views, once it has
  any. A hostname routes to one service and fails on its own (DNS not
  pointed yet, certificate pending), so the row counts the ones in trouble
  ("1 pending"). It is one row rather than one per hostname because a
  hostname does not fit the sidebar at that depth, and a name cut short is
  not shown. Its name and state are never cut either: when both do not fit,
  the state goes under the name. The row opens the Domains page, where each
  hostname is read in full. A service with no custom domain has no row;
  adding one is the Domains tab of its Overview.
- **The server's domain records** (the certificates and DNS zones under
  Domains today, which several services can share) are not entities of one
  service. They move to the Server page with the other server-wide lists.
- **Its databases are not repeated here.** They are rows of the project, one
  home each. The Overview says which databases the service uses and links to
  them.

### Where the other views go

The occasional views become tabs of the everyday page they belong to, by the
question they answer:

| Row | Tabs | Moved in |
|---|---|---|
| Overview ("is it healthy?") | Overview, Activity, Metrics ▾, Uptime ▾, Alerts ▾, Domains, Databases ▾ | Metrics, Uptime, Alerts, Domains, Databases |
| Deployments | Deployments, Environments, Build | |
| Logs ("what happened?") | Application, Requests, OpenTelemetry, Traces ▾ | Traces |
| Errors | Issues, Feedback, Releases, Settings ▾ | |
| Variables | Variables, Secrets, Deploy tokens | |
| Analytics | Overview, Visitors ▾, Pages ▾, Funnels ▾, Revenue, Settings ▾ | |
| Settings | General, Data ingest, Protection, Edge rules, Access, Scans, Automation, Integrations | |

Nothing is removed: every page keeps its URL and stays in ⌘K. What changes is
that a service shows seven rows instead of thirteen, and a view that is not
used yet (Traces before the first trace) is a tab with its set-up state, not
an "Add" entry in the tree.

### With a hundred projects

The Projects branch lists, in this order, at most eight:

1. projects with something down or failing;
2. projects opened recently in this browser.

Then "All projects (100)", which opens the Projects page with search. The
project being shown is always in the list. With eight projects or fewer, all
are listed, as today. A project the user wants every day, they pin.

## Your own sidebar: pins

The tree is the same for everyone, so the product, the docs and a colleague
all describe one shape. What a user makes their own is a short list of
**pins** above it: shortcuts to what they open every day, wherever it sits
in the tree. This is the first step of the direction we started from, where
users pin what matters into their own views.

**What can be pinned.** Anything with a page:

- a thing: a project, a service, a database, a custom domain, a server;
- one view of a thing: "storefront-api · Logs", "web · Analytics";
- something saved: a saved log search, a metrics dashboard, a funnel.

**How it reads.** A pin shows the same name and live state as the thing
itself (a pinned service that goes down says "Down" in Pinned too). A view's
pin is named by its thing and its view ("api · Logs"), never by a label the
user typed, so a renamed service renames its pins. The page a pin opens is
marked in Pinned and in the tree.

**How to pin.**

- A **Pin** button in the header of every page that can be pinned. It is
  where the task happens, so the feature is found without being explained.
- **Pin** and **Unpin** in a row's menu in the tree, and on a phone in the
  same menu (long press on a row).
- Drag to reorder in Pinned; Unpin from the same menu.

**What a user cannot change**, and why:

- **The tree's rows, their order, or hiding them.** Hiding a row would hide a
  feature (the repository rule is never to hide one), and every user's tree
  would differ from the docs and from what a colleague describes. The tree
  already adapts on its own: a view appears once it holds something, a project
  in trouble rises to the top.
- **Renaming a pin.** A typed label goes stale when the thing is renamed and
  stops matching what the rest of the console calls it.

**Bounds.** At most 12 pins; beyond that, a dashboard is the better tool, and
the Pin button says so. A pin to something deleted disappears with it. A pin
to something the user can no longer read is not shown (and comes back if
access returns).

**Where pins live.** On the server, per user, so they follow the user to
another browser and to their phone: one row per pin (user, kind of target,
target id, the view's path when it is a view, position), written through a
typed endpoint for the signed-in user only, and audit-logged like every
write. The console reads them in one request with the tree, and each pin
resolves its name and state from its target, so nothing is copied.

**Later, not in this step:** pins an admin sets for the whole team (the
production project, the on-call dashboard), shown above a user's own and not
removable by them.

### On a phone

The same tree is the drawer. The page's button names the row shown
("storefront-web · Logs") and opens the drawer on it, as built in
revision 4.

## Compared with today

| | Today | Proposed |
|---|---|---|
| Top-level branches | 4 (Projects, Databases, Domains, Servers) | 1 (Projects), plus the Server row |
| Rows under an open service | up to 13 | at most 7, plus its domains |
| Where a database is listed | the Databases branch | its project |
| Where a service's custom domain is listed | its Domains view | one Domains row under the service, and the Domains tab of its Overview |
| Where the server's domain records (certificates, DNS) are listed | the Domains branch | a tab of the Server page |
| Server-wide database and domain lists | top-level branches | tabs of the Server page |
| A view not set up yet | under "Add" in the tree | a tab, showing its set-up state |
| What a user can change | the fold of each branch | the fold, plus up to 12 pins that follow them across devices |

## Open questions

1. **Custom domains under the service, or under the project?** Built under
   the service, as one Domains row. A project with many services and
   domains might want them together.
2. **Metrics, Uptime and Alerts as tabs of Overview.** They answer "is it
   healthy?", but Alerts also holds rules that are configured, not looked at.
   Should alert rules move to Settings and only firing alerts stay on the
   Overview?
3. **The Server row for non-admins.** A member who cannot manage nodes sees
   nothing useful there. Hide the row, or show only the lists they can read?
   Built: the row is shown to everyone and says nothing about a state it
   could not read.
4. **The tab strip's "More".** It is an overflow of the same row of tabs on a
   narrow screen, not a menu category, but it uses a word the inventory bans
   for menu entries. Keep it, or show the overflow count instead ("+4")?
5. **Recent projects** are remembered per browser. Pins follow the user;
   should recent projects too?
6. **Two meanings of "Pin".** Saved log searches already have a Pin that
   puts a search at the top of its service's list, for everyone. A sidebar
   pin is personal. Rename the existing one ("Keep at top"), or make pinning
   a saved search mean the sidebar pin?
7. **Team pins.** Worth building with the first step, or only once users
   have pins of their own?

## What was built

Steps 1 to 4 are built, with these choices where the proposal left one open:

- **The Server row** opens on the nodes. Its page's tabs are Nodes (Nodes,
  This server), Databases (Databases, Backups) and Domains (Domains,
  Certificates, DNS providers, Delivery), each a tab that opens a menu of
  its pages. It says the worst of the nodes and databases, counted.
- **A project's databases** are asked for only while the project is open,
  at most 25 services across every open project; one past that says "All
  databases" and links to the project's Databases page. A database page
  is marked under the open project that lists it, and on the Server row
  otherwise.
- **A service's views** are Overview, Deployments, Logs, Errors, Variables,
  Analytics and Settings. The Overview's tabs are Overview, Activity,
  Metrics, Uptime, Alerts, Domains and Databases; Logs' are Application,
  Requests, OpenTelemetry and Traces. The quick tour points at the Metrics
  and Traces tabs.
- **With more than eight projects**, the list is the one shown, those in
  trouble (listed first), those opened recently in this browser, then the
  rest in order, up to eight, followed by "All projects (N)".

The order they were planned in, each step shippable:

1. Projects branch only at the top, the Server row and the server page tabs
   (Nodes, Databases, Domains, Backups).
2. A project's databases as rows under it, and "+ Add".
3. A service's rows cut to the everyday seven, its domains under it, and the
   occasional views moved into the tabs listed above.
4. The capped project list for large servers.
5. Pins: the table and endpoint, Pinned in the tree, the Pin button in page
   headers and the row menu.

Every move keeps the old URL working, ⌘K finds every page, and the e2e
navigation spec is updated with it.
