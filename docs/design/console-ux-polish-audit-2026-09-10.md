# Console UX polish audit — 10 September 2026

Scope: current `chore/temps-work-20260909` worktree, desktop first. This is a read-only source audit of route definitions, page composition, visible copy, controls, and conditional empty states. It is not a claim that every page was opened in an authenticated browser or that every workflow was executed. The local frontend now targets the remote instance; no production forms or actions were submitted for this review. Earlier screenshots informed the analytics/logs findings, but are not proof of other pages' runtime appearance.

The feedback is accurate: the visual foundation is usable. The largest opportunities are consistent presentation, direct next actions, and removing text that describes implementation or repeats a label. A new visual system is unnecessary.

Priority: **P1** affects finding a task, understanding data, or completing a common workflow; **P2** is presentation/copy polish. These are UX priorities, not security severities. Findings below describe current source; proposed changes are recommendations, not implemented work.

## Findings: global operations and integrations

| Priority | Page / flow | Current evidence | Recommended change |
|---|---|---|---|
| P1 | Databases → create | `web/src/pages/Storage.tsx:168` says “Databases,” but tabs at 181–185 say “Platform Services / External Services”; `CreateServiceNew.tsx:476` says “Create Service” and 612 labels “Service Name.” | Keep “Databases” through listing, creation, linking, and success messages. Use user-understandable grouping for application databases versus Temps' own data stores; do not relabel infrastructure inaccurately. |
| P1 | Email | `web/src/pages/Email.tsx:18` defaults to Providers even for configured installations. Page heading at 35 is followed by another large heading/description in `components/email/EmailProvidersManagement.tsx:972`. | Default configured installations to sent mail/activity, and unconfigured ones to setup. One page heading; subordinate section title only when useful. Keep provider/domain setup available. |
| P1 | Email domains empty state | `components/email/EmailDomainsManagement.tsx:491–496` tells users to “Go to the Providers tab” with no action. | “Add an email provider” button linking directly to provider creation. Keep one sentence explaining the dependency. |
| P1 | Revenue empty state | `pages/Revenue.tsx:346–358` tells users to connect Stripe or LemonSqueezy on a project; only the filtered-empty branch has an action. | Provide a project picker followed by a direct billing-integration link. Keep Clear filters for filtered results. |
| P1 | Certificates empty state | `pages/Certificates.tsx:162–164` says “Enable on-demand TLS in settings” without a setup action. | Add the exact settings link and distinguish no issuance attempts from TLS being unconfigured. |
| P1 | AI workflows | `/ai-workflows` and `/agent-sandbox` both use “AI Workflows” (`pages/AiWorkflowsOverview.tsx:38`, `pages/agent-sandbox/AgentSandboxLayout.tsx:52`) but one is a project chooser and the other infrastructure settings. `AiWorkflowsOverview.tsx:63` gives create-project directions without a CTA. | Name the destinations by task (“Workflows” and “AI runtime settings”), and provide Create project from the empty state. Avoid requiring users to infer which hub they need. |
| P2 | Tools directory | `pages/PlatformTools.tsx:72`: “Every Temps capability remains available here while the main sidebar stays focused on daily work.” | Remove this explanation of the navigation design. Title + search + grouped links are enough. |
| P2 | Add backup storage | `pages/CreateS3Source.tsx:138–154` stacks “Add S3 Source,” a description, “S3 Configuration,” and another instruction paragraph. | One heading (“Add backup storage”), fields, and help only for non-obvious S3 options. Keep endpoint examples and credential guidance that prevent errors. |
| P2 | Database create-and-link | `pages/CreateServiceNew.tsx:599–603` explains provisioning runtime variables and updating the application sandbox network “as one operation.” | Show “Will be linked to [project]” with the selected environment mode. Put provisioning details in expandable help. |
| P2 | Cluster replica creation | `pages/AddClusterMember.tsx:229–232`: “The role reconciler refreshes role-aliased VIP records on its next tick.” | “Add a replica to this database cluster.” Show actual role, target node, progress, and availability consequences; remove reconciler implementation details from the main form. |
| P2 | Upgrade details | `pages/MajorUpgradeDetail.tsx:352` explains idempotency and `:373` labels “JSONL log stream.” | Prefer “Retry resumes from the failed step” and “Logs.” Preserve phase status, errors, and retry semantics. |
| P2 | Backup run/details | `pages/ScheduleRunDetail.tsx:422` says “scheduler tick”; `pages/BackupDetail.tsx:613` leads with “provenance.” | “Backups in this run”; “Backup details.” Put backup status, completion time, target, and restore action before secondary metadata. |
| P2 | Restore | `pages/ServiceRestore.tsx:947` describes the “restore orchestrator.” | “Review restore plan.” Keep the explicit overwrite, target database, downtime, and foreign-backup warnings at 748 and 913. Less text must not mean less informed consent. |
| P2 | AI gateway/setup | `pages/AiGateway.tsx:4200–4227` repeats the endpoint pitch; `AiGatewaySetupPage.tsx:116–149` repeats endpoint/quick-start instructions, then always shows BYOK detail at 184. | Configured landing page: provider health and key actions. Setup: endpoint + one working snippet. Put per-request BYOK instructions under Advanced. |
| P2 | AI workspace creation | `components/ai-first/AiFirstWorkspace.tsx:2688` explains harness/persistent sandbox/role inheritance; 3144 and 3174 add more implementation copy around creating a workspace. | Lead with name, start-from source, and assistant choice. Retain a short permission/scope statement; move runtime and Autopack explanations into optional help. |
| P2 | Agent runtime settings | `pages/agent-sandbox/AgentSandboxLayout.tsx:54` explains that each surface “owns its own status and settings”; provider detail at 505 explains AES-256-GCM and relay mechanics. | Remove architecture explanations from headings. Keep credential scope/security information concise and contextual to the credential field. |
| P2 | Git/DNS providers | `pages/GitSources.tsx:180`, `DnsProviders.tsx:157` use custom headers; failed-load copy at 209 / 184 says try later/contact support. | Shared page header and direct Retry. Keep provider type, connection health, last sync, and Add provider prominent. |
| P2 | Domain/certificate presentation | `pages/Certificates.tsx:115` has a multi-line ACME implementation introduction; `DomainDetail.tsx:1782` repeats healthy HTTPS status in an alert. | Put certificate status and expiry in the summary. Show detailed challenge guidance only when setup/renewal needs action. Preserve actionable renewal failure and DNS warnings. |

## Shared rules to apply

1. One page title. Descriptions are optional, not required boilerplate.
2. One prominent next action for an empty state. A sentence telling users where to go should usually become a link/button.
3. Distinguish never configured, no data in this range, no filter matches, and failed loading.
4. Put everyday controls first; use Advanced for uncommon inputs and optional implementation details.
5. Reuse an entire dashboard's composition and data presentation, not just its tab strip or chart wrapper.
6. Maintain a small, consistent vocabulary across sidebar, headings, fields, actions, and notifications.
7. Keep consequential explanations: deletion, restore overwrite/downtime, permission scope, required DNS changes, and the meaning of ambiguous measurements.
8. Keep operational evidence visible: current status, affected resource, timestamp, error cause, and the action to recover.

## Findings: project workflows

| Priority | Page / flow | Current evidence | Recommended change |
|---|---|---|---|
| P1 | Build & deploy settings | `components/project/settings/CombinedProjectSettings.tsx:57,147–185` puts six sections in disclosures, only the first initially open. Child `BuildDeploySettings.tsx:47` renders another title/description inside the titled disclosure. | Keep the small settings sidebar, but show the common Source → Build → Deploy sequence directly. Use disclosures for advanced options, not every section. Remove duplicate child headings. A shared Save bar requires careful dirty-state and endpoint handling, not a visual-only change. |
| P1 | New project | `components/project/NewProjectShell.tsx:85,104,145` gives six source choices equal tab prominence alongside provider status chips. | Emphasize repository import and templates; group less common sources without removing access. Put provider identity next to repository selection. |
| P1 | Template configuration | `components/templates/TemplateConfigurator.tsx:749–813` places metadata, tags/features/requirements, then “Project Configuration / Configure your new project” before the actual form. Later sections include runtime, Git, databases, variables. | Compact template identity; required project name/inputs first; optional customization second; a clear “Will create” summary. Preserve useful template requirements and resource consequences. |
| P1 | Project database linking | `components/project/ProjectStorage.tsx:111–124,217–232` makes an unlinked row navigate to global details while a separate Link button performs the local task. | Clearly separate “Link” from “View details”; use a title link for details and the explicit Link action. Preserve the user-requested provider cards, existing-database choices, and per-environment/project/custom modes. Do not hide the creation gallery merely to reduce density. |
| P1 | Project overview | `components/project/ProjectOverview.tsx:301–359` puts deployment summaries after analytics/other panels; `ProjectDetailHeader.tsx:113–218` already exposes deploy/status. | Lead with what is running: production environment, deployed revision, health, live URL. Keep one canonical Deploy action and a short recent-history list; secondary analytics below. |
| P1 | Security disabled state | `pages/security/SecurityOverview.tsx:235–292,372–395` shows a scanning-disabled explanation then disabled scan actions with another setup tooltip. | One “Scanning is off” state with Enable scanning; keep environment status visible without repeating the same blocker per panel. |
| P2 | General settings | `CombinedProjectSettings.tsx:48–55,147–185` wraps “Project settings” around `GeneralSettings.tsx:213–258`, which repeats heading and helpers, including telemetry naming semantics. | Direct Name/Slug fields and save action. Put rename consequences where they affect the decision rather than in the permanent page introduction. |
| P2 | Upload source | `pages/ProjectDrop.tsx:274–321` uses “Repository optional,” “Deployment card,” explanatory paragraphs, then “Configure upload.” | “Upload source” + folder/ZIP control + environment + Deploy. Keep immutable setting constraints near the relevant fields. |
| P2 | Security environment selection | `SecurityOverview.tsx:252–282` uses tabs with status/severity badges while Environments uses `EnvironmentNavigation.tsx:50–108`. | Reuse the environment selector where comparison is not the primary task; reserve badges for the active scan summary. |

The environment mini-sidebar, status dot in the switcher, and explicit database-linking modes reflect already accepted product choices. This audit does not recommend undoing them. Deployment actions should be consistent without adding a second competing primary button.

## Findings: observability

| Priority | Page / flow | Current evidence | Recommended change |
|---|---|---|---|
| P1 | Global versus project analytics | `pages/observability/GlobalAnalytics.tsx` still supplies a separate Breakdown renderer; `components/project/ProjectAnalytics.tsx:1476` supplies the richer project-specific cards. Summary/chart/tabs and technology icons are shared, but the dashboard composition and breakdown behavior are not. | Complete the previously requested shared dashboard. Scope-specific data adapters should feed the same cards, presentation, empty states, and interactions. Do not claim visual parity just because individual primitives are shared. |
| P1 | Analytics search scope | Global analytics places search in the main toolbar, but only Breakdown queries use it (`GlobalAnalytics.tsx:37–50`); summary and timeline queries omit it (`:90–105`). | Move “Filter breakdown rows” into the breakdown section, or make its scope consistent across compatible data. Do not imply the headline metrics represent the search. |
| P1 | Project traces filters | `pages/TracesList.tsx:1147–1293` combines date, status, environment, deployment, service, ID, span name, and attribute controls in one filter area. | Keep common scope/time/search visible; advanced filters on demand with active chips. Match the compact filtering convention already requested for Logs. |
| P1 | Global project selector | `components/observability/GlobalPage.tsx:70–116` loads 100 choices and separately paginates the choice catalog (“Project choices…”). | Searchable project combobox with asynchronous results and pinned All projects. Users should search names, not paginate an option list. |
| P1 | Global trace click destination | `GlobalTraces.tsx:109–126` makes the title open project detail and adds a separate “Cross-project waterfall” link on each row. | On the global page, make the primary trace link open the global waterfall; project identity links to the project. |
| P1 | Global empty states | `GlobalPage.tsx:261–271` supplies generic guidance and a Projects destination across surfaces. | Per-surface empty actions for not configured, no data, no matches; preserve errors separately. |
| P1 | Monitoring navigation | `components/monitoring/monitoring-sections.ts:4–9` has Alerts, Alert rules, Alarms, Notifications; shared title remains “Monitoring & Alerts” (`components/monitoring/MonitoringSettings.tsx:1164–1203`). | Make the relationship explicit: rules define conditions, active alerts show incidents, notification destinations deliver them. Use current-section titles, not the same generic intro everywhere. |
| P1 | Monitoring rules | `MonitoringSettings.tsx:1026–1033` stacks infrastructure and project error-rule systems; `AlertRulesManagement.tsx:100–121,190–224` redirects into project pages after project selection. | Explicit sections with scope and counts; keep the destination predictable. Avoid adding another navigation tier unless needed. |
| P2 | Logs toolbar/status | `LogExplorer.tsx:163–240` shows result count and many equal-weight utilities; `GlobalLogs.tsx:203–209` repeats the loaded count. “Paused” at 141–158 describes a state before the user has started live mode. | Count once; clear Live on/off action; group copy/export/presentation utilities. Keep query, time, live state, and results dominant. |
| P2 | Logs scan-limit copy | `GlobalLogs.tsx:80–87` says “No complete result page could be established…” | “Shorten the time range or add a filter to search these logs.” Preserve the indication that the results are incomplete. |
| P2 | Project analytics onboarding | `ProjectAnalytics.tsx:1371–1393` uses a yellow warning card for “No analytics data detected yet.” | Neutral setup state and “Set up analytics.” Missing instrumentation is not automatically a failure. |
| P2 | Project errors | `components/projects/ErrorTracking.tsx:846–855,963–976` repeats no-error states, including “Your application is running smoothly”; tabs at 858–883 are local state. | One evidence-based state (“No errors received”); setup when required. Make selected subpage URL-addressable for refresh/back/share. |
| P2 | Descriptive headers | Proxy Logs uses “Advanced… comprehensive…” (`pages/ProxyLogs.tsx:20–26`); Global Errors explains table counting in the page description (`GlobalErrors.tsx:49–55`). | Short task labels; put metric definitions on column help. Remove promotional adjectives. |

Already corrected and not outstanding: duplicate Backups landing header, first-time traces onboarding followed by an empty list, logs chart/hover styling, analytics technology icons. The broader analytics parity issue remains open. Audience/Technology are already behind tabs; no additional collapse is needed merely for its own sake.

## Findings: administration and settings

| Priority | Page / flow | Current evidence | Recommended change |
|---|---|---|---|
| P1 | Notification delete | `components/monitoring/NotificationRoutesManagement.tsx:131–138` directly calls delete from a trash icon; `ProvidersManagement.tsx:409–414` calls delete from a menu without confirmation. | Confirm the named destination/provider and consequence, as other resource deletion flows do. This is interaction safety, not merely copy polish. |
| P1 | API key create | `pages/ApiKeyCreate.tsx:327–417,642–706` uses three steps for name/expiry, permissions, review. Clickable role divs at 459–507 lack radio/keyboard semantics; role and custom permission choices compete. | One compact name/expiry/role form, expandable custom permissions, semantic RadioGroup. Keep one-time secret presentation and scope consequences clear. |
| P1 | Platform settings | `pages/Settings.tsx:183–310,374–427` combines public URL, HTTPS, internal URL, ACME with long helper text. | Group by outcome; short helpers plus contextual details. Make unsaved changes and save scope clear. Preserve proxy-loop and access implications near affected changes. |
| P1 | Teams navigation/access | `pages/Teams.tsx:341–345` and `TeamDetail.tsx:628–636` navigate through mouse-only table rows. TeamDetail 598–616 explains granting project access elsewhere without a direct action. | Name links supporting keyboard/new-tab; “Grant project access” with project selection. |
| P1 | Plugins | `pages/settings/PluginsPage.tsx:72–175,213–332` puts installation guidance, examples, and terminal steps around the installed-plugin interface. | Installed plugins/status first; Add/install action reveals instructions. Keep documentation accessible without displaying it permanently. |
| P2 | Settings navigation | `components/settings/settings-navigation.ts:47–126` exposes 23 destinations; operational monitoring sits with Security. | Clear Operations grouping for monitoring/pipeline/delivery; Security for protection/access. Keep short labels and existing shallow sidebar structure. |
| P2 | API key list | `pages/ApiKeys.tsx:115–161` allocates three summary cards to total/active/expiring before the table. | Compact filter counts, with Expiring actionable. Keep key name, role/scope, last used, expiry, revoke accessible. |
| P2 | User details | `pages/UserDetail.tsx:271–347` emphasizes four activity metrics ahead of account administration. | Account identity/role/team/security actions first; usage/history secondary. Escalate failed-login counts only when meaningful. |
| P2 | Login | `pages/Login.tsx:142–148` and `components/auth/login-form.tsx:134–140` repeat title and instructions. | One “Sign in to Temps” title. Preserve SSO, password recovery, and verification clarity. |
| P2 | Notifications headings | `pages/Notifications.tsx:29–48` already provides title/tabs; management panels repeat large headings and descriptions (`ProvidersManagement.tsx:335–355`, `NotificationRoutesManagement.tsx:65–83`). | Panel toolbar + rows; provider-specific constraints only in the relevant form. |
| P2 | Cloud connection | `pages/settings/CloudSettingsPage.tsx:222–225,475–501` repeats data/trust explanations; 436–473 numbers a one-field action. | Enrollment code + Connect instance; one concise data-scope note and optional details. Keep explicit export switches and connected status. |
| P2 | Worker nodes | `pages/settings/NodesPage.tsx:1576–1605` combines join tokens/nodes, DNS, and emergency trust management. | Separate clearly named Nodes / Networking / Cluster trust sections. Keep trust rotation confirmations intact. |

## Detail-page findings

| Priority | Page | Evidence | Recommendation |
|---|---|---|---|
| P1 | Session replays | `components/analytics/SessionReplays.tsx:169–179` suggests visits alone produce recordings; error recovery at 115–119 reloads the app. | Distinguish capture disabled from no recordings in range; link to setup; query retry instead of full reload. |
| P1 | Web performance | `components/project/ProjectSpeedInsights.tsx:622–699` stacks setup cards, “Automatic Web Vitals Tracking,” and “Real User Monitoring” explanations. | One compact setup state and CTA; metric explanations in contextual help. |
| P1 | Proxy Logs advanced filters | `components/proxy-logs/ProxyLogsDataTable.tsx:832–1300` exposes 20+ fields in one grid after opening advanced filters. | Keep the disclosure; group HTTP, client/bot, routing, and performance filters, with useful presets. Do not put all fields back into the default view. |
| P1 | Trace detail parity/deep links | `pages/CrossProjectTraceDetail.tsx:250–290` and `TraceDetail.tsx:857–883` use different summaries for overlapping data; TraceDetail tabs at 918–945 use local default state. | Shared trace detail presentation; URL-backed Spans/Logs tab. |
| P1 | Error event recovery | `pages/ErrorEventDetail.tsx:146–171` lacks a recovery CTA for not-found; its back icon lacks an explicit accessible name. | “Back to error group” link/button with preserved group context. |
| P2 | Metrics Explorer | `pages/MetricsExplorer.tsx:1225–1230`: “Series are bucketed server-side…” | “Select a metric to plot it over time.” The URL filters/correlation structure is otherwise worth preserving. |
| P2 | Trace Operations | `pages/TraceOperations.tsx:373–382` mentions server logs and “span-stats query.” | Contextual failure + Retry; technical diagnostics available separately. |
| P2 | AI crawler detail | `components/analytics/AiAgentsDetail.tsx:417–456` repeats explanatory subtitles on adjacent cards. | Remove subtitles that restate titles; keep help for ambiguous classifications such as crawl purpose. |
| P2 | Pages / recordings headers | `components/analytics/Pages.tsx:156–190`, `SessionReplays.tsx:129–147` use extra card headers/date descriptions around the parent analytics scope. | Compact toolbar with range/count when needed; avoid competing page-level headings. |
| P1 | Revenue KPI meaning | `pages/Revenue.tsx:247–254` shows “Paid in view” based on the first 200 loaded events. | Use a true aggregate for a headline KPI, or explicitly call it “Paid in loaded events.” This is a data interpretation issue, not a copy-only clean-up. |
| P2 | Request detail | `pages/RequestLogDetail.tsx:136` starts with generic “Request Log Detail.” | Lead with METHOD + path, response status, time, and host; metadata beneath. |

## Coverage by page family

“Source review” means route/component/copy/conditional-flow inspection, not an executed workflow. “Sampled” means route and representative page internals were checked but not every child control. Absence of a listed issue is not a runtime certification. Dynamic plugin-provided UI cannot be exhaustively reviewed from the host route.

| Area / routes | Coverage | Result |
|---|---|---|
| Main shell, sidebar, project sidebar, settings sidebar, tools directory | Source review | Vocabulary, overcrowded settings groups, explanatory tools intro. |
| Projects list, overview, project setup | Source review | Make running deployment/health first; preserve existing actions. |
| New project, repository import, public Git, Docker image, templates/services, upload/drop | Source review | Choice hierarchy, long template form, repeated framing. |
| Deployments list/detail | Source review | Wording/action consistency; retain canonical Deploy action. No separate redesign justified. |
| Environments, containers, environment settings | Source review | Keep mini-sidebar/status-dot approach; standardize secondary metadata/action labels. |
| Project databases, existing links, creation modes | Source review | Separate row navigation from Link; preserve all provider cards/modes. |
| Security overview/scans/vulnerability details, protection/access | Source review + detail samples | Disabled state and environment selection consistency. Security remains its own menu item. |
| Project General, Build & deploy, Variables, Automation, Integrations, Telemetry | Source review | Nested closed sections, duplicate headings, implementation helpers. |
| Project agents/autofixer, revenue integrations | Representative source review | Include in settings hierarchy/copy pass; do not collapse consequential controls. |
| Global/project analytics overview | Source review; earlier screenshots available | Shared composition still incomplete; search scope misleading. |
| Visitors/live visitors, pages/page detail, funnels/create/edit/detail, segments/journey/globes | Representative source review | No additional major flow issue identified; minor duplicated toolbars. Live data/3D behavior untested. |
| Sessions/replay details | Source review + detail samples | Onboarding truthfulness, filtering context, recovery. Playback untested. |
| Web performance | Source review | Overbuilt setup state. |
| AI visitors/crawlers/activity, API traffic, revenue | Source review + representative detail samples | Repeated subtitles; Revenue aggregate scope. |
| Global/application/project/service logs | Source review + representative detail samples | Live label, utility density, repeated counts. Shared chart fix already applied. |
| Request/proxy logs and details | Source review | Advanced filter grouping and request-identity hierarchy. |
| Traces global/project/detail/cross-project/operations | Source review | Click destinations, scope parity, dense filters, URL tab state. |
| Errors groups/events/analytics/source maps/setup | Source review + detail samples | Duplicate/misleading empty states, missing recovery link, URL state. |
| Monitoring alerts/rules/alarms/delivery, metric alert forms | Source review | Overlapping names and rule-system scope. |
| Metrics explorer, dashboards/list/builder/view | Source review + builder sample | Explorer intro copy; create/edit empty actions are a good pattern. |
| Proxy metrics, IP geolocation detail | Proxy source review; IP route/sample | Proxy hierarchy is reasonable; IP detail needs runtime visual pass before conclusions. |
| Global Databases, create/import/detail | Source review | “Services” vocabulary drift; order operational facts before metadata. |
| Database browser/query performance/monitoring/logs | Representative source review | No additional major presentation issue established; data browsing needs populated runtime checks. |
| Restore/cluster member/major upgrade | Source review | Replace internal terms; preserve downtime/overwrite/trust consequences. |
| Backups, S3 source/add/detail, schedules/create/edit/detail, runs, backup detail | Source review | Source-centric wording, repeated form intro, scheduler terminology. Landing duplicate title already fixed. |
| Domains/add/detail, DNS providers/add/detail, Certificates | Source review | Setup action links and certificate introduction; preserve actionable DNS guidance. |
| Git providers/add/detail | Source review | Header/copy and error recovery consistency. |
| Email providers/domains/sent/analytics/SDK and detail/create forms | Source review | Configured landing, double headings, missing provider CTA. SDK instructions belong in SDK view. |
| AI workspace/chat/new workspace | Source review of entry/creation composition | Reduce harness/runtime explanation; shared chat body is a good reuse pattern. Conversation execution untested. |
| AI gateway/providers/setup/usage/activity | Source review | Setup content competes with configured use; keep advanced BYOK optional. |
| AI onboarding/harness connection | Source review | Keep verification/credential scope; shorten repeated setup descriptions. |
| AI workflows, agent sandbox overview/providers/detail/runtime/preview/secrets | Source review + preview/secrets component samples | Duplicate hub name, implementation copy. Secret injection instructions are useful where configuring references. |
| Sandboxes/list/detail | Source review | Preserve distinction between managed workspaces and standalone resources; shorten mechanism copy. |
| Global Settings | Source review | Density, grouping, save scope. |
| Account/profile/password/MFA/sessions | Source review | No major workflow problem found; helpers can be shorter. |
| Users/create/detail/edit, Teams/detail | Source review | User detail hierarchy, keyboard links, direct access-grant action. |
| API keys/create/detail/edit | Source review | Wizard friction, role accessibility; preserve one-time secrets and revoke confirmation. |
| Login, forgot/reset/required change, MFA, CLI login | Login/reset detailed; other verification source samples | Duplicate sign-in intro; password recovery is focused and worth preserving. Authentication not exercised remotely. |
| Auth/OIDC configuration/create/detail | Representative source review | Shared header/helper pass; no new high-priority issue established. |
| Notifications/providers/create/edit/routes/create/edit | Source review | Delete confirmation, duplicate headings, provider detail copy. |
| Load balancer/custom routes/create | Route and form source sample | Include in shared header/form pass; live routing not exercised. |
| Docker registry, version | Representative source review | Compact copy/shared headers; no major flow issue established. |
| Security/rate limiting/build limits/timeouts | Representative source review | Keep meaningful limits/consequences; trim redundant card introductions. |
| Disk/metrics monitoring, OTel pipeline, Traefik discovery | Representative source review | Operations grouping and compact status-first presentation. |
| Cloud connection, worker nodes/detail | Source review + node-detail sample | Setup/trust copy, long combined operations page; preserve explicit export/rotation controls. |
| Plugins/host plugin page | Host source review | Installed-first hierarchy; external plugin content not audited. |
| Skills/MCP catalogs/details/server | Representative source review | No new high-priority issue established; instructional content is appropriate in setup/detail contexts. |
| Audit logs | Route/source sample | Preserve filter/table approach; include shared empty/error/header pass. |
| Legacy redirects and Not Found | Route inventory | Verify destinations during future browser pass; do not count redirects as additional feature pages. |

## Suggested implementation order

1. Fix interaction safety and misleading data: notification deletion, keyboard-operable role/team controls, analytics search scope, Revenue's loaded-event KPI.
2. Complete analytics/trace shared dashboard/detail composition; finish filter consistency for Traces and Proxy Logs.
3. Simplify project settings and project/template creation. Preserve fewer primary links, the mini-sidebar, independent Security, and database linking modes.
4. Give every blocked/empty state its direct next action: Email, Revenue, Certificates, Workflows, Replays, global observability.
5. Apply one compact header/form/list pattern to Email, Backups, providers, API keys, Users, and Notifications.
6. Trim setup prose in Plugins, Cloud, AI gateway/workspace/runtime, and advanced infrastructure forms.

Validate each batch with configured, unconfigured, filtered-empty, error, and populated states on desktop; then dark mode, keyboard, and mobile. Do not deploy UI changes or exercise destructive actions against the remote instance as part of a visual audit.

## Source page inventory

The inventory below records all 151 non-test TSX files under `web/src/pages` at review time. Some are wrappers, legacy surfaces, or supporting pages rather than independently reachable routes. The coverage table above states the depth of review; this inventory alone is not a claim of exhaustive runtime testing.

- `web/src/pages/Account.tsx`
- `web/src/pages/AddClusterMember.tsx`
- `web/src/pages/AddDnsProvider.tsx`
- `web/src/pages/AddDomain.tsx`
- `web/src/pages/AddEmailProvider.tsx`
- `web/src/pages/AddGitProvider.tsx`
- `web/src/pages/AddNotificationProvider.tsx`
- `web/src/pages/AddRoute.tsx`
- `web/src/pages/AiChat.tsx`
- `web/src/pages/AiFirstPrototype.tsx`
- `web/src/pages/AiGateway.tsx`
- `web/src/pages/AiGatewayActivityPage.tsx`
- `web/src/pages/AiGatewaySetupPage.tsx`
- `web/src/pages/AiGatewayUsagePage.tsx`
- `web/src/pages/AiOnboarding.tsx`
- `web/src/pages/AiWorkflowsOverview.tsx`
- `web/src/pages/Alarms.tsx`
- `web/src/pages/AlertRuleForm.tsx`
- `web/src/pages/AlertsRouter.tsx`
- `web/src/pages/ApiKeyCreate.tsx`
- `web/src/pages/ApiKeyDetail.tsx`
- `web/src/pages/ApiKeyEdit.tsx`
- `web/src/pages/ApiKeys.tsx`
- `web/src/pages/AuditLogs.tsx`
- `web/src/pages/BackupDetail.tsx`
- `web/src/pages/Backups.tsx`
- `web/src/pages/Certificates.tsx`
- `web/src/pages/CliLogin.tsx`
- `web/src/pages/ContainerDetailPage.tsx`
- `web/src/pages/CreateBackupSchedule.tsx`
- `web/src/pages/CreateFunnel.tsx`
- `web/src/pages/CreateS3Source.tsx`
- `web/src/pages/CreateServiceNew.tsx`
- `web/src/pages/CreateUser.tsx`
- `web/src/pages/CrossProjectTraceDetail.tsx`
- `web/src/pages/DashboardBuilder.tsx`
- `web/src/pages/DashboardView.tsx`
- `web/src/pages/Dashboards.tsx`
- `web/src/pages/DashboardsRouter.tsx`
- `web/src/pages/DeploymentDetails.tsx`
- `web/src/pages/DnsProviderDetail.tsx`
- `web/src/pages/DnsProviders.tsx`
- `web/src/pages/DomainDetail.tsx`
- `web/src/pages/Domains.tsx`
- `web/src/pages/Drop.tsx`
- `web/src/pages/EditBackupSchedule.tsx`
- `web/src/pages/EditFunnel.tsx`
- `web/src/pages/EditNotificationProvider.tsx`
- `web/src/pages/Email.tsx`
- `web/src/pages/EmailDetail.tsx`
- `web/src/pages/EmailDomainDetail.tsx`
- `web/src/pages/EmailDomainNew.tsx`
- `web/src/pages/EmailProviderDetail.tsx`
- `web/src/pages/EnvironmentDashboard.tsx`
- `web/src/pages/EnvironmentsTabsView.tsx`
- `web/src/pages/ErrorEventDetail.tsx`
- `web/src/pages/ErrorGroupDetail.tsx`
- `web/src/pages/ForgotPassword.tsx`
- `web/src/pages/GitProviderDetail.tsx`
- `web/src/pages/GitSources.tsx`
- `web/src/pages/Import.tsx`
- `web/src/pages/ImportProject.tsx`
- `web/src/pages/ImportService.tsx`
- `web/src/pages/IpGeolocationDetail.tsx`
- `web/src/pages/LiveVisitors.tsx`
- `web/src/pages/Login.tsx`
- `web/src/pages/LogsList.tsx`
- `web/src/pages/MajorUpgradeDetail.tsx`
- `web/src/pages/MetricAlertForm.tsx`
- `web/src/pages/MetricAlerts.tsx`
- `web/src/pages/Metrics.tsx`
- `web/src/pages/MetricsExplorer.tsx`
- `web/src/pages/MfaVerify.tsx`
- `web/src/pages/Monitoring.tsx`
- `web/src/pages/NewProject.tsx`
- `web/src/pages/NotificationRouteForm.tsx`
- `web/src/pages/Notifications.tsx`
- `web/src/pages/Observe.tsx`
- `web/src/pages/PlatformTools.tsx`
- `web/src/pages/ProjectAiCrawlers.tsx`
- `web/src/pages/ProjectDetail.tsx`
- `web/src/pages/ProjectDrop.tsx`
- `web/src/pages/ProjectSetup.tsx`
- `web/src/pages/Projects.tsx`
- `web/src/pages/ProxyLogDetail.tsx`
- `web/src/pages/ProxyLogs.tsx`
- `web/src/pages/ProxyMetrics.tsx`
- `web/src/pages/RequestLogDetail.tsx`
- `web/src/pages/RequestLogs.tsx`
- `web/src/pages/RequestLogsList.tsx`
- `web/src/pages/RequiredPasswordChange.tsx`
- `web/src/pages/ResetPassword.tsx`
- `web/src/pages/Revenue.tsx`
- `web/src/pages/Routes.tsx`
- `web/src/pages/S3SourceDetail.tsx`
- `web/src/pages/SandboxDetail.tsx`
- `web/src/pages/Sandboxes.tsx`
- `web/src/pages/ScheduleDetail.tsx`
- `web/src/pages/ScheduleRunDetail.tsx`
- `web/src/pages/ServiceDataBrowser.tsx`
- `web/src/pages/ServiceDetail.tsx`
- `web/src/pages/ServiceLogs.tsx`
- `web/src/pages/ServiceMonitoring.tsx`
- `web/src/pages/ServiceQueryPerformance.tsx`
- `web/src/pages/ServiceRestore.tsx`
- `web/src/pages/SessionReplayDetail.tsx`
- `web/src/pages/Settings.tsx`
- `web/src/pages/Setup.tsx`
- `web/src/pages/Storage.tsx`
- `web/src/pages/TeamDetail.tsx`
- `web/src/pages/Teams.tsx`
- `web/src/pages/TraceDetail.tsx`
- `web/src/pages/TraceOperations.tsx`
- `web/src/pages/Traces.tsx`
- `web/src/pages/TracesList.tsx`
- `web/src/pages/UserDetail.tsx`
- `web/src/pages/Users.tsx`
- `web/src/pages/agent-sandbox/AgentSandboxDashboard.tsx`
- `web/src/pages/agent-sandbox/AgentSandboxLayout.tsx`
- `web/src/pages/agent-sandbox/AgentSandboxPreviewPage.tsx`
- `web/src/pages/agent-sandbox/AgentSandboxProviderDetail.tsx`
- `web/src/pages/agent-sandbox/AgentSandboxProvidersList.tsx`
- `web/src/pages/agent-sandbox/AgentSandboxSandboxPage.tsx`
- `web/src/pages/agent-sandbox/AgentSandboxSecretsPage.tsx`
- `web/src/pages/observability/GlobalAnalytics.tsx`
- `web/src/pages/observability/GlobalErrors.tsx`
- `web/src/pages/observability/GlobalLogs.tsx`
- `web/src/pages/observability/GlobalTraces.tsx`
- `web/src/pages/plugins/PluginPage.tsx`
- `web/src/pages/security/ScanDetail.tsx`
- `web/src/pages/security/SecurityOverview.tsx`
- `web/src/pages/security/VulnerabilityDetailPage.tsx`
- `web/src/pages/settings/AuthSettingsPage.tsx`
- `web/src/pages/settings/BuildLimitsPage.tsx`
- `web/src/pages/settings/CloudSettingsPage.tsx`
- `web/src/pages/settings/CreateOidcProviderPage.tsx`
- `web/src/pages/settings/DiskMonitoringPage.tsx`
- `web/src/pages/settings/DockerRegistryPage.tsx`
- `web/src/pages/settings/GlobalMcpServerDetail.tsx`
- `web/src/pages/settings/GlobalSkillDetail.tsx`
- `web/src/pages/settings/McpServerPage.tsx`
- `web/src/pages/settings/MonitoringSettingsPage.tsx`
- `web/src/pages/settings/NodesPage.tsx`
- `web/src/pages/settings/OidcProviderDetailPage.tsx`
- `web/src/pages/settings/OtelPipelineStatusPage.tsx`
- `web/src/pages/settings/PluginsPage.tsx`
- `web/src/pages/settings/RateLimitingPage.tsx`
- `web/src/pages/settings/RequestTimeoutsPage.tsx`
- `web/src/pages/settings/SecurityPage.tsx`
- `web/src/pages/settings/TraefikDiscoveryPage.tsx`
- `web/src/pages/settings/VersionPage.tsx`
