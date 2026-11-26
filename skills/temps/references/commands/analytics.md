<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `analytics` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`analytics keys`](#analytics-keys)
- [`analytics conversions`](#analytics-conversions-alias-goals)
- [`analytics identity`](#analytics-identity)
- [`analytics overview`](#analytics-overview-alias-o)
- [`analytics top`](#analytics-top)
- [`analytics outbound`](#analytics-outbound)
- [`analytics downloads`](#analytics-downloads)
- [`analytics not-found`](#analytics-not-found)
- [`analytics funnels`](#analytics-funnels)
- [`analytics performance`](#analytics-performance-alias-speed)
- [`analytics ai-agents`](#analytics-ai-agents)
- [`analytics ai-pages`](#analytics-ai-pages)
- [`analytics ai-page`](#analytics-ai-page)
- [`analytics api-overview`](#analytics-api-overview)
- [`analytics api-routes`](#analytics-api-routes)
- [`analytics api-callers`](#analytics-api-callers)
- [`analytics api-ip`](#analytics-api-ip)
- [`analytics api-path`](#analytics-api-path)
- [`analytics api-query`](#analytics-api-query)
- [`analytics api-summary`](#analytics-api-summary)
- [`analytics enrich`](#analytics-enrich)
- [`analytics identify`](#analytics-identify)
- [`analytics get-active-visitors`](#analytics-get-active-visitors)
- [`analytics get-event-detail`](#analytics-get-event-detail)
- [`analytics get-event-entries`](#analytics-get-event-entries)
- [`analytics get-event-visitors`](#analytics-get-event-visitors)
- [`analytics get-events-count`](#analytics-get-events-count)
- [`analytics get-stats`](#analytics-get-stats)
- [`analytics get-global`](#analytics-get-global)
- [`analytics get`](#analytics-get)
- [`analytics check-has-events`](#analytics-check-has-events)
- [`analytics get-live-visitors-list`](#analytics-get-live-visitors-list)
- [`analytics get-page-flow`](#analytics-get-page-flow)
- [`analytics get-page-hourly-sessions`](#analytics-get-page-hourly-sessions)
- [`analytics get-page-path-detail`](#analytics-get-page-path-detail)
- [`analytics get-page-path-visitors`](#analytics-get-page-path-visitors)
- [`analytics get-page-paths-sparklines`](#analytics-get-page-paths-sparklines)
- [`analytics get-recent-activity`](#analytics-get-recent-activity)
- [`analytics get-session-details`](#analytics-get-session-details)
- [`analytics get-session-events`](#analytics-get-session-events)
- [`analytics get-session-logs`](#analytics-get-session-logs)
- [`analytics get-visitor-facets`](#analytics-get-visitor-facets)
- [`analytics get-visitors`](#analytics-get-visitors)
- [`analytics get-visitor-by-guid`](#analytics-get-visitor-by-guid)
- [`analytics get-visitor-by-id`](#analytics-get-visitor-by-id)
- [`analytics get-visitor-details`](#analytics-get-visitor-details)
- [`analytics identify-visitor-for-token`](#analytics-identify-visitor-for-token)
- [`analytics get-visitor-info`](#analytics-get-visitor-info)
- [`analytics get-visitor-journey`](#analytics-get-visitor-journey)
- [`analytics get-visitor-sessions`](#analytics-get-visitor-sessions)
- [`analytics get-visitor-stats`](#analytics-get-visitor-stats)
- [`analytics events`](#analytics-events)
- [`analytics visitors`](#analytics-visitors)
- [`analytics settings`](#analytics-settings)
- [`analytics get-api-callers`](#analytics-get-api-callers)
- [`analytics get-api-routes`](#analytics-get-api-routes)

## `analytics` (alias: `stats`)

View project analytics

**Subcommands:**

- `keys` - Manage analytics ingest keys (pa_...) for apps Temps does not deploy
- `conversions` (`goals`) - Conversion goals: events or pages that count as success
- `identity` - Whether visitors are recognised by a stored id or tracked cookieless, and who may identify them
- `overview` (`o`) - Show analytics dashboard overview
- `top` - Show breakdown by dimension: pages, referrers, browsers, os, devices, countries, regions, cities, channels, events, languages, utm_source, utm_medium, utm_campaign
- `outbound` - Show the outbound links visitors clicked (outbound_click events)
- `downloads` - Show the files visitors downloaded (file_download events)
- `not-found` - Show pages visitors could not find: 404s Temps answered and not_found events
- `funnels` - Show funnel conversion metrics for all funnels
- `performance` (`speed`) - Show real-user Web Vitals and optional dimension breakdowns
- `ai-agents` - Show AI crawler / provider breakdown (web /analytics/ai-agents)
- `ai-pages` - Show pages crawled by AI agents, with distinct-agent counts
- `ai-page` - Show which agents/providers crawled a single page (e.g. /docs)
- `api-overview` - Show API traffic timeseries (requests, errors, latency) from /api-analytics/timeseries
- `api-routes` - Show top API routes by request count from /api-analytics/routes
- `api-callers` - Show top API callers by client IP from /api-analytics/callers
- `api-ip` - Show routes called by one client IP with latency and error analytics
- `api-path` - Show client IPs calling one path with latency and error analytics
- `api-query` - Run a typed multi-dimensional API traffic aggregation
- `api-summary` - Show an AI-generated summary of API traffic from /api-analytics/summary (requires AI Assistance to be configured and enabled on the project)
- `enrich` - Attach identity or attributes to a visitor (merges into custom_data; a null value removes a key)
- `identify` - Label a visitor with your app's user id, as your backend would (browser identify cannot replace it)
- `get-active-visitors` - Get detailed active visitors
- `get-event-detail` - Get detailed analytics for a specific event
- `get-event-entries` - Get paginated list of raw occurrences of a specific event, including custom JSON properties
- `get-event-visitors` - Get paginated list of visitors who triggered a specific event
- `get-events-count` - Get analytics events count
- `get-stats` - Get general statistics across all projects for a time frame
- `get-global` - Get global analytics
- `get` - Get analytics projects
- `check-has-events` - Check analytics has events
- `get-live-visitors-list` - Get list of currently live visitors from visitor table
- `get-page-flow` - Get page flow analytics: entry pages, exit pages, drop-off points, and page transitions
- `get-page-hourly-sessions` - Get page hourly sessions
- `get-page-path-detail` - Get detailed analytics for a specific page path Returns visitors, page views, activity over time,..
- `get-page-path-visitors` - Get individual visitor sessions for a specific page path
- `get-page-paths-sparklines` - Get page paths sparklines
- `get-recent-activity` - Get recent activity events for real-time activity feed
- `get-session-details` - Get detailed information about a specific session including events and request logs
- `get-session-events` - Get analytics session events
- `get-session-logs` - Get session logs
- `get-visitor-facets` - Get filter dropdown contents for the visitors page
- `get-visitors` - Get list of visitors with summary information
- `get-visitor-by-guid` - Get visitor by GUID with geolocation data
- `get-visitor-by-id` - Get visitor by numeric ID with geolocation data
- `get-visitor-details` - Get detailed information about a specific visitor by numeric ID
- `identify-visitor-for-token` - Identify a visitor of the token's own project (ADR 109 D4)
- `get-visitor-info` - Get visitor record from database
- `get-visitor-journey` - Get the complete visitor journey: all events across all sessions, grouped by session
- `get-visitor-sessions` - Get all sessions for a specific visitor by numeric ID
- `get-visitor-stats` - Get visitor statistics
- `events` - Events commands
- `visitors` - Visitors commands
- `settings` - Settings commands
- `get-api-callers` - Return the top callers (by client IP, ranked by request count) in a project's API traffic window
- `get-api-routes` - Return the top routes (by request count) in a project's API traffic, grouped by raw '(method, path)'

### `analytics keys`

Manage analytics ingest keys (pa_...) for apps Temps does not deploy

**Subcommands:**

- `get` - Show one analytics ingest key
- `list` (`ls`) - List analytics ingest keys for a project
- `create` (`add`) - Mint a new analytics ingest key
- `update` - Update an ingest key's label, origin allowlist, or rate limit
- `rotate` - Replace an ingest key value, keeping the same row and scope
- `revoke` - Revoke (deactivate) an analytics ingest key
- `get-ingest-endpoint` - The public address an app Temps does not deploy should send analytics to

#### `analytics keys get`

Show one analytics ingest key

**Usage:** `analytics keys get <key-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics keys list` (alias: `ls`)

List analytics ingest keys for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics keys create` (alias: `add`)

Mint a new analytics ingest key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-n, --name <name>` | Operator-facing label for the key | - | No |
| `--environment-id <id>` | Scope the key to one environment (omit for a project-wide key) | - | No |
| `--allowed-origins <origins...>` | Browser origins allowed to use this key (omit to allow any origin) | - | No |
| `--rate-limit <n>` | Requests per minute (omit for the server default; 0 or less for unlimited) | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics keys update`

Update an ingest key's label, origin allowlist, or rate limit

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--key-id <id>` | Analytics ingest key ID | - | Yes |
| `-n, --name <name>` | New operator-facing label | - | No |
| `--allowed-origins <origins...>` | Replace the origin allowlist with these origins | - | No |
| `--clear-origins` | Clear the origin allowlist (allow any origin) | - | No |
| `--rate-limit <n>` | New requests-per-minute limit | - | No |
| `--clear-rate-limit` | Clear the rate limit (unlimited) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics keys rotate`

Replace an ingest key value, keeping the same row and scope

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--key-id <id>` | Analytics ingest key ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics keys revoke`

Revoke (deactivate) an analytics ingest key

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--key-id <id>` | Analytics ingest key ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

#### `analytics keys get-ingest-endpoint`

The public address an app Temps does not deploy should send analytics to

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics conversions` (alias: `goals`)

Conversion goals: events or pages that count as success

**Subcommands:**

- `list` - List conversions with visitors, rate and revenue in a period
- `create` - Define a conversion
- `update` - Replace a conversion's definition
- `delete` - Delete a conversion (recorded events are kept)
- `breakdown` - Where the visitors who converted came from
- `get-goal` - Get one conversion goal

#### `analytics conversions list`

List conversions with visitors, rate and revenue in a period

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 7d, 30d, 3m) | `30d` | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

#### `analytics conversions create`

Define a conversion

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `-n, --name <name>` | Name shown in reports, e.g. Signup | - | No |
| `--event <name>` | Count this event, e.g. signup | - | No |
| `--page <path>` | Count visits to this path; * matches anything, e.g. /docs/* | - | No |
| `--property <key=value>` | Only events whose property has this value, e.g. plan=pro | - | No |
| `--revenue-property <key>` | Event property holding the amount, e.g. amount | - | No |
| `--currency <code>` | ISO 4217 currency of the amount, e.g. EUR | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics conversions update`

Replace a conversion's definition

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--goal-id <id>` | Conversion ID (see list) | - | Yes |
| `-n, --name <name>` | Name shown in reports, e.g. Signup | - | No |
| `--event <name>` | Count this event, e.g. signup | - | No |
| `--page <path>` | Count visits to this path; * matches anything, e.g. /docs/* | - | No |
| `--property <key=value>` | Only events whose property has this value, e.g. plan=pro | - | No |
| `--revenue-property <key>` | Event property holding the amount, e.g. amount | - | No |
| `--currency <code>` | ISO 4217 currency of the amount, e.g. EUR | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics conversions delete`

Delete a conversion (recorded events are kept)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--goal-id <id>` | Conversion ID (see list) | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation (alias for --force) | - | No |

#### `analytics conversions breakdown`

Where the visitors who converted came from

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--goal-id <id>` | Conversion ID (see list) | - | Yes |
| `--by <dimension>` | One of channel, referrer, utm_source, utm_medium, utm_campaign, page, country, device_type, browser, operating_system | `channel` | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 7d, 30d, 3m) | `30d` | No |
| `--limit <n>` | Rows (default: 10, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

#### `analytics conversions get-goal`

Get one conversion goal

**Usage:** `analytics conversions get-goal <goal-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics identity`

Whether visitors are recognised by a stored id or tracked cookieless, and who may identify them

**Subcommands:**

- `show` - Show the identity mode, whether browser identify is accepted, and what they change
- `set` - Switch between persistent and cookieless tracking, or accept/refuse browser identify

#### `analytics identity show`

Show the identity mode, whether browser identify is accepted, and what they change

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics identity set`

Switch between persistent and cookieless tracking, or accept/refuse browser identify

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--mode <mode>` | One of persistent, cookieless | - | No |
| `--browser-identify <accept\|refuse>` | Whether identify() from the browser is applied; refuse leaves identify to the app's backend | - | No |
| `-y, --yes` | Skip confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics overview` (alias: `o`)

Show analytics dashboard overview

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |
| `--deployment-id <n>` | Only this deployment (an ID) | - | No |

### `analytics top`

Show breakdown by dimension: pages, referrers, browsers, os, devices, countries, regions, cities, channels, events, languages, utm_source, utm_medium, utm_campaign

**Usage:** `analytics top <dimension> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--limit <n>` | Number of results (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |
| `--deployment-id <n>` | Only this deployment (an ID) | - | No |
| `--filter-browser <value>` | Only visitors on this browser | - | No |
| `--filter-channel <value>` | Only visitors from this channel (e.g. Organic Search) | - | No |
| `--filter-country <value>` | Only visitors from this country | - | No |
| `--filter-os <value>` | Only visitors on this operating system | - | No |
| `--filter-referrer <value>` | Only visitors from this referrer | - | No |
| `--filter-region <value>` | Only visitors from this region | - | No |
| `--include-crawlers` | Count crawler traffic too | - | No |
| `--no-include-crawlers` | Set include_crawlers to false | - | No |

### `analytics outbound`

Show the outbound links visitors clicked (outbound_click events)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `7d` | No |
| `--limit <n>` | Number of results (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

### `analytics downloads`

Show the files visitors downloaded (file_download events)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `7d` | No |
| `--limit <n>` | Number of results (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

### `analytics not-found`

Show pages visitors could not find: 404s Temps answered and not_found events

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `7d` | No |
| `--limit <n>` | Number of results (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

### `analytics funnels`

Show funnel conversion metrics for all funnels

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `7d` | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |
| `--country-code <value>` | Only visitors from this country (ISO code, e.g. DE) | - | No |

### `analytics performance` (alias: `speed`)

Show real-user Web Vitals and optional dimension breakdowns

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (ignored with --start-date/--end-date) | `7d` | No |
| `--start-date <date>` | Explicit window start (RFC 3339; requires --end-date) | - | No |
| `--end-date <date>` | Explicit window end (RFC 3339; requires --start-date) | - | No |
| `--environment-id <id>` | Restrict samples to one environment ID | - | No |
| `--deployment-id <id>` | Restrict samples to one deployment ID | - | No |
| `--device <device>` | Device filter: desktop or mobile | - | No |
| `--include-bots` | Include crawler and datacenter bot samples | - | No |
| `--group-by <dimension>` | Break down by path, country, region, city, device_type, browser, or operating_system | - | No |
| `--path <path>` | Restrict samples to one page pathname | - | No |
| `--country <country>` | Restrict samples to one country | - | No |
| `--region <region>` | Restrict samples to one region | - | No |
| `--city <city>` | Restrict samples to one city | - | No |
| `--browser <browser>` | Restrict samples to one browser | - | No |
| `--os <operating-system>` | Restrict samples to one operating system | - | No |
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `has-metrics` - Check if performance metrics exist for a project

#### `analytics performance has-metrics`

Check if performance metrics exist for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics ai-agents`

Show AI crawler / provider breakdown (web /analytics/ai-agents)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d) | `24h` | No |
| `--limit <n>` | Number of rows to fetch (default: 20, max: 100) | - | No |
| `--group-by <mode>` | Group rows by "agent" (default) or "provider" | `agent` | No |
| `--path <path>` | Restrict to one URL path (e.g. /docs) | - | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

### `analytics ai-pages`

Show pages crawled by AI agents, with distinct-agent counts

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d) | `24h` | No |
| `--limit <n>` | Number of pages to fetch (default: 20, max: 100) | - | No |
| `--path <path>` | Restrict to one URL path (returns just that row) | - | No |
| `--with-agents` | Also fetch and render the per-agent split for each page (slower) | - | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

### `analytics ai-page`

Show which agents/providers crawled a single page (e.g. /docs)

**Usage:** `analytics ai-page <path> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d) | `24h` | No |
| `--limit <n>` | Number of rows to fetch (default: 50, max: 100) | - | No |
| `--group-by <mode>` | Group rows by "agent" (default) or "provider" | `agent` | No |
| `--json` | Output in JSON format | - | No |
| `--environment-id <n>` | Only this environment of the project (an ID) | - | No |

### `analytics api-overview`

Show API traffic timeseries (requests, errors, latency) from /api-analytics/timeseries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-routes`

Show top API routes by request count from /api-analytics/routes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--limit <n>` | Number of routes to return (default: 20, max: 100) | - | No |
| `--offset <n>` | Number of ranked routes to skip (default: 0) | - | No |
| `--sort-by <metric>` | Sort by requests, latency_avg, or error_rate | `requests` | No |
| `--order <direction>` | Sort direction: asc or desc | `desc` | No |
| `--include-synthetic` | Include Temps status-monitor checks | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-callers`

Show top API callers by client IP from /api-analytics/callers

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--limit <n>` | Number of callers to return (default: 20, max: 100) | - | No |
| `--offset <n>` | Number of ranked callers to skip (default: 0) | - | No |
| `--sort-by <metric>` | Sort by requests or error_rate | `requests` | No |
| `--order <direction>` | Sort direction: asc or desc | `desc` | No |
| `--include-synthetic` | Include Temps status-monitor checks | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-ip`

Show routes called by one client IP with latency and error analytics

**Usage:** `analytics api-ip <ip> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period (e.g. 1h, 24h, 7d, 30d) | `24h` | No |
| `--limit <n>` | Rows per page (default: 20, max: 100) | - | No |
| `--page <n>` | Page number | `1` | No |
| `--sort-by <metric>` | Sort by requests, latency_avg, or error_rate | `requests` | No |
| `--order <direction>` | Sort direction: asc or desc | `desc` | No |
| `--include-synthetic` | Include Temps status-monitor checks | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-path`

Show client IPs calling one path with latency and error analytics

**Usage:** `analytics api-path <path> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period (e.g. 1h, 24h, 7d, 30d) | `24h` | No |
| `--limit <n>` | Rows per page (default: 20, max: 100) | - | No |
| `--page <n>` | Page number | `1` | No |
| `--sort-by <metric>` | Sort by requests or error_rate | `requests` | No |
| `--order <direction>` | Sort direction: asc or desc | `desc` | No |
| `--include-synthetic` | Include Temps status-monitor checks | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-query`

Run a typed multi-dimensional API traffic aggregation

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--group-by <dimensions>` | Comma-separated dimensions (omit for one overall rollup) | - | No |
| `--metrics <metrics>` | Comma-separated metrics (e.g. requests,error_rate,latency_p95) | - | Yes |
| `--filter <dimension:operator:value>` | Repeatable filter (operators: eq, not_eq, contains, starts_with, in) | `` | No |
| `--sort-by <field>` | Requested dimension or metric to sort by | - | No |
| `--order <direction>` | Sort direction: asc or desc | `desc` | No |
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period (e.g. 1h, 24h, 7d, 30d) | `24h` | No |
| `--page <n>` | Page number | `1` | No |
| `--limit <n>` | Rows per page (default: 20, max: 100) | - | No |
| `--include-synthetic` | Include Temps status-monitor checks | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics api-summary`

Show an AI-generated summary of API traffic from /api-analytics/summary (requires AI Assistance to be configured and enabled on the project)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--refresh` | Generate a fresh summary instead of the cached one | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics enrich`

Attach identity or attributes to a visitor (merges into custom_data; a null value removes a key)

**Usage:** `analytics enrich <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-d, --data <json>` | JSON object to merge, e.g. '{"user_id":"user_123"}' | - | No |
| `-f, --file <path>` | Read the JSON object to merge from a file | - | No |
| `--set <key=value>` | Set one key to a string value (repeatable; use --data for numbers, booleans, or nested values) | `` | No |
| `--unset <key>` | Remove one top-level key (repeatable; sends null) | `` | No |
| `--json` | Output in JSON format | - | No |

### `analytics identify`

Label a visitor with your app's user id, as your backend would (browser identify cannot replace it)

**Usage:** `analytics identify <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--user-id <id>` | Your app's id for this person (at most 256 characters) | - | Yes |
| `-d, --data <json>` | Traits as a JSON object, e.g. '{"plan":"pro"}' | - | No |
| `--set <key=value>` | Set one trait to a string value (repeatable) | `` | No |
| `--unset <key>` | Remove one trait (repeatable; sends null) | `` | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-active-visitors`

Get detailed active visitors

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--deployment-id <n>` | Deployment ID (optional) | - | No |
| `--window-minutes <n>` | Time window in minutes for active visitors (default: 5) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-event-detail`

Get detailed analytics for a specific event

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--event-name <value>` | Event name to get details for [required] | - | No |
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date (ISO 8601) [required] | - | No |
| `--end-date <value>` | End date (ISO 8601) [required] | - | No |
| `--bucket-interval <value>` | Bucket interval: hour, day, week, month (default: auto) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-event-entries`

Get paginated list of raw occurrences of a specific event, including custom JSON properties

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--event-name <value>` | Event name to list occurrences for [required] | - | No |
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date (ISO 8601) [required] | - | No |
| `--end-date <value>` | End date (ISO 8601) [required] | - | No |
| `--page <n>` | Page number (1-based, default: 1) | - | No |
| `--per-page <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-event-visitors`

Get paginated list of visitors who triggered a specific event

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--event-name <value>` | Event name to list visitors for [required] | - | No |
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date (ISO 8601) [required] | - | No |
| `--end-date <value>` | End date (ISO 8601) [required] | - | No |
| `--page <n>` | Page number (1-based, default: 1) | - | No |
| `--per-page <n>` | Items per page (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-events-count`

Get analytics events count

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-date <value>` | Start date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--end-date <value>` | End date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--limit <n>` | Maximum number of results to return | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--custom-events-only` | Only return custom events, excluding system events like page_view, page_leave, heartbeat (default: true) | - | No |
| `--no-custom-events-only` | Set custom_events_only to false | - | No |
| `--breakdown <value>` | Breakdown by geography: 'country', 'region', or 'city' (optional) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-stats`

Get general statistics across all projects for a time frame

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-date <value>` | Start date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--end-date <value>` | End date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--project-ids <n>` | Optional: Filter by specific project IDs (comma-separated) (repeatable) | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--include-project-breakdown` | Whether to include per-project breakdown (default: false) | - | No |
| `--no-include-project-breakdown` | Set include_project_breakdown to false | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-global`

Get global analytics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--facet <value>` | Query parameter facet (one of: summary, traffic, pages, events, breakdown, speed) | - | No |
| `--project-id <n>` | Query parameter project_id | - | No |
| `--environment-id <n>` | Query parameter environment_id | - | No |
| `--start-date <value>` | Query parameter start_date [required] | - | No |
| `--end-date <value>` | Query parameter end_date [required] | - | No |
| `--search <value>` | Query parameter search | - | No |
| `--dimension <value>` | Query parameter dimension | - | No |
| `--device <value>` | Query parameter device | - | No |
| `--sort-by <value>` | Query parameter sort_by | - | No |
| `--sort-order <value>` | Query parameter sort_order | - | No |
| `--page <n>` | Query parameter page | - | No |
| `--per-page <n>` | Query parameter per_page | - | No |
| `--min-views <n>` | Query parameter min_views | - | No |
| `--min-sessions <n>` | Query parameter min_sessions | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get`

Get analytics projects

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `analytics check-has-events`

Check analytics has events

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-live-visitors-list`

Get list of currently live visitors from visitor table

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--window-minutes <n>` | Time window in minutes for live visitors (default: 5) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-page-flow`

Get page flow analytics: entry pages, exit pages, drop-off points, and page transitions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date in ISO 8601 format [required] | - | No |
| `--end-date <value>` | End date in ISO 8601 format [required] | - | No |
| `--limit <n>` | Max entry/exit pages to return (default: 20, max: 100) | - | No |
| `--transitions-limit <n>` | Max page transitions to return (default: 50, max: 200) | - | No |
| `--min-views-for-dropoff <n>` | Minimum views for drop-off analysis (default: 5) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-page-hourly-sessions`

Get page hourly sessions

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page-path <value>` | The page path to get sessions for [required] | - | No |
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-time <value>` | Start time in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--end-time <value>` | End time in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--bucket-interval <value>` | Bucket interval: 'hour', 'day', 'week', or 'month' (default: auto-determined based on range) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-page-path-detail`

Get detailed analytics for a specific page path Returns visitors, page views, activity over time,..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page-path <value>` | The page path to get details for (URL-encoded) [required] | - | No |
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date in ISO 8601 format [required] | - | No |
| `--end-date <value>` | End date in ISO 8601 format [required] | - | No |
| `--bucket-interval <value>` | Bucket interval for time series: 'hour', 'day', 'week', 'month' (default: auto based on date range) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-page-path-visitors`

Get individual visitor sessions for a specific page path

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--page-path <value>` | The page path to get visitors for (URL-encoded) [required] | - | No |
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date in ISO 8601 format [required] | - | No |
| `--end-date <value>` | End date in ISO 8601 format [required] | - | No |
| `--page <n>` | Page number (1-based, default: 1) | - | No |
| `--per-page <n>` | Items per page (default: 50, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-page-paths-sparklines`

Get page paths sparklines

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-time <value>` | Start time in ISO 8601 format [required] | - | No |
| `--end-time <value>` | End time in ISO 8601 format [required] | - | No |
| `--page-paths <value>` | Comma-separated list of page paths [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-recent-activity`

Get recent activity events for real-time activity feed

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--since-id <n>` | Return events with ID greater than this (cursor-based polling) | - | No |
| `--limit <n>` | Max events to return (default: 50, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-session-details`

Get detailed information about a specific session including events and request logs

**Usage:** `analytics get-session-details <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-session-events`

Get analytics session events

**Usage:** `analytics get-session-events <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date in format YYYY-MM-DD HH:MM:SS | - | No |
| `--end-date <value>` | End date in format YYYY-MM-DD HH:MM:SS | - | No |
| `--limit <n>` | Number of results to return (default: 100) | - | No |
| `--offset <n>` | Number of results to skip (default: 0) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-session-logs`

Get session logs

**Usage:** `analytics get-session-logs <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Start date in format YYYY-MM-DD HH:MM:SS | - | No |
| `--end-date <value>` | End date in format YYYY-MM-DD HH:MM:SS | - | No |
| `--limit <n>` | Number of results to return (default: 100) | - | No |
| `--offset <n>` | Number of results to skip (default: 0) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-facets`

Get filter dropdown contents for the visitors page

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-date <value>` | Start date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--end-date <value>` | End date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--include-crawlers` | Include crawlers (default: false) | - | No |
| `--no-include-crawlers` | Set include_crawlers to false | - | No |
| `--has-activity-only` | Hide ghost visitors (default: true) | - | No |
| `--no-has-activity-only` | Set has_activity_only to false | - | No |
| `--per-facet-limit <n>` | Top N values per dimension (default: 50, max: 200) | - | No |
| `--filter-country <value>` | Geolocation country | - | No |
| `--filter-region <value>` | Geolocation region | - | No |
| `--filter-city <value>` | Geolocation city | - | No |
| `--filter-channel <value>` | First-touch channel | - | No |
| `--filter-referrer <value>` | First-touch referrer hostname (use 'Direct' for null) | - | No |
| `--filter-user-id <value>` | User id set by identify(): the same user on every device | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitors`

Get list of visitors with summary information

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-date <value>` | Start date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--end-date <value>` | End date in format YYYY-MM-DD HH:MM:SS [required] | - | No |
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--include-crawlers` | Include crawlers (default: false) | - | No |
| `--no-include-crawlers` | Set include_crawlers to false | - | No |
| `--limit <n>` | Maximum number of visitors to return (default: 50) | - | No |
| `--offset <n>` | Number of visitors to skip (default: 0) | - | No |
| `--has-activity-only` | Filter to only include visitors with recorded activity (events/sessions) | - | No |
| `--no-has-activity-only` | Set has_activity_only to false | - | No |
| `--filter-country <value>` | Geolocation country | - | No |
| `--filter-region <value>` | Geolocation region | - | No |
| `--filter-city <value>` | Geolocation city | - | No |
| `--filter-channel <value>` | First-touch channel | - | No |
| `--filter-referrer <value>` | First-touch referrer hostname (use 'Direct' for null) | - | No |
| `--filter-user-id <value>` | User id set by identify(): the same user on every device | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-by-guid`

Get visitor by GUID with geolocation data

**Usage:** `analytics get-visitor-by-guid <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-by-id`

Get visitor by numeric ID with geolocation data

**Usage:** `analytics get-visitor-by-id <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-details`

Get detailed information about a specific visitor by numeric ID

**Usage:** `analytics get-visitor-details <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics identify-visitor-for-token`

Identify a visitor of the token's own project (ADR 109 D4)

**Usage:** `analytics identify-visitor-for-token <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--traits <json>` | Traits merged into the visitor's 'custom_data' at the top level; a key set to 'null' is removed (JSON) | - | No |
| `--user-id <value>` | The app's own id for the person (database id, UUID, email) [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-info`

Get visitor record from database

**Usage:** `analytics get-visitor-info <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-journey`

Get the complete visitor journey: all events across all sessions, grouped by session

**Usage:** `analytics get-visitor-journey <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--limit-sessions <n>` | Maximum number of sessions to return (default: 50) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-sessions`

Get all sessions for a specific visitor by numeric ID

**Usage:** `analytics get-visitor-sessions <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--limit <n>` | Maximum number of sessions to return (default: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-visitor-stats`

Get visitor statistics

**Usage:** `analytics get-visitor-stats <visitor-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID or slug [required] | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics events`

Events commands

**Subcommands:**

- `get-dashboard` - Get dashboard analytics for multiple projects in a single batch request
- `get-active-visitors` - Get active visitors count
- `get-type-breakdown` - Get event type breakdown
- `record-console` - Record an analytics event via the console API with explicit project ID
- `get-property-timeline` - Get property timeline by grouping events by a column over time
- `get-timeline` - Get events timeline
- `has` - Check if project has any analytics events
- `get-hourly-visits` - Get hourly visits
- `get-session` - Get events for a specific session

#### `analytics events get-dashboard`

Get dashboard analytics for multiple projects in a single batch request

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-ids <value>` | Comma-separated list of project IDs [required] | - | No |
| `--start-date <value>` | Start date for filtering [required] | - | No |
| `--end-date <value>` | End date for filtering [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events get-active-visitors`

Get active visitors count

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--deployment-id <n>` | Filter by deployment ID | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events get-type-breakdown`

Get event type breakdown

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--start-date <value>` | Start date for filtering events [required] | - | No |
| `--end-date <value>` | End date for filtering events [required] | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--aggregation-level <value>` | Aggregation level: events, sessions, or visitors (default: events) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events record-console`

Record an analytics event via the console API with explicit project ID

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--deployment-id <n>` | Deployment ID to attribute the event to [required] | - | No |
| `--environment-id <n>` | Environment ID to attribute the event to [required] | - | No |
| `--event-data <json>` | Arbitrary JSON event data (JSON) | - | No |
| `--event-name <value>` | Event name (e.g. "purchase", "signup", custom event names) [required] | - | No |
| `--request-path <value>` | Page path context (defaults to "/") | - | No |
| `--request-query <value>` | Query string context | - | No |
| `--session-id <value>` | Encrypted '_temps_sid' cookie value from the user's browser | - | No |
| `--visitor-id <value>` | Encrypted '_temps_visitor_id' cookie value from the user's browser | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events get-property-timeline`

Get property timeline by grouping events by a column over time

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--start-date <value>` | Start date in '%Y-%m-%d %H:%M:%S' format [required] | - | No |
| `--end-date <value>` | End date in '%Y-%m-%d %H:%M:%S' format [required] | - | No |
| `--group-by <value>` | Column to group by (channel, device_type, browser, etc.) [required] | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--deployment-id <n>` | Filter by deployment ID | - | No |
| `--event-name <value>` | Filter by event name | - | No |
| `--aggregation-level <value>` | Aggregation level: events, sessions, or visitors - default: events | - | No |
| `--bucket-size <value>` | Time bucket: hour, day, week, month (default: auto-detect) | - | No |
| `--include-crawlers` | Include crawler/bot traffic (default: false) | - | No |
| `--no-include-crawlers` | Set include_crawlers to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events get-timeline`

Get events timeline

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--start-date <value>` | Start date for filtering events [required] | - | No |
| `--end-date <value>` | End date for filtering events [required] | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--event-name <value>` | Filter by specific event name | - | No |
| `--bucket-size <value>` | Bucket size: hour, day, or week (auto-detected if not specified) | - | No |
| `--aggregation-level <value>` | Aggregation level: events, sessions, or visitors (default: events) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events has`

Check if project has any analytics events

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events get-hourly-visits`

Get hourly visits

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--start-date <value>` | Start date for filtering visits [required] | - | No |
| `--end-date <value>` | End date for filtering visits [required] | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--aggregation-level <value>` | Aggregation level: events (page views), sessions (unique sessions), or visitors (unique visitors) -... | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics events get-session`

Get events for a specific session

**Usage:** `analytics events get-session <session-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics visitors`

Visitors commands

**Subcommands:**

- `get-activity-status` - Get activity status
- `save-activity-settings` - Save activity settings
- `suggest-activity-goals` - Suggest activity goals
- `preview-activity-report` - Preview activity report
- `run-activity-report` - Run activity report

#### `analytics visitors get-activity-status`

Get activity status

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Query parameter environment_id | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics visitors save-activity-settings`

Save activity settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--application-context <value>` | Body field application_context [required] | - | No |
| `--categories <json>` | Body field categories (JSON) [required] | - | No |
| `--daily-enabled` | Body field daily_enabled [required] | - | No |
| `--no-daily-enabled` | Set daily_enabled to false | - | No |
| `--environment-id <n>` | Body field environment_id | - | No |
| `--goal-title <value>` | Short operator-facing label for the selected analysis goal | - | No |
| `--min-page-paths <n>` | Body field min_page_paths | - | No |
| `--min-sessions <n>` | Body field min_sessions | - | No |
| `--property-keys <value>` | Only these event-property keys may be sent to the provider (repeatable) [required] | - | No |
| `--share-activity-with-ai` | Explicit permission to send the selected analytics fields to the configured AI provider [required] | - | No |
| `--no-share-activity-with-ai` | Set share_activity_with_ai to false | - | No |
| `--source-domain <value>` | Body field source_domain | - | No |
| `--source-url <value>` | Body field source_url | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics visitors suggest-activity-goals`

Suggest activity goals

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Body field environment_id | - | No |
| `--share-with-ai` | Body field share_with_ai [required] | - | No |
| `--no-share-with-ai` | Set share_with_ai to false | - | No |
| `--url <value>` | Body field url [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics visitors preview-activity-report`

Preview activity report

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Body field environment_id | - | No |
| `--goal <value>` | Body field goal [required] | - | No |
| `--min-page-paths <n>` | Body field min_page_paths | - | No |
| `--min-sessions <n>` | Body field min_sessions | - | No |
| `--property-keys <value>` | Body field property_keys (repeatable) | - | No |
| `--share-activity-with-ai` | Body field share_activity_with_ai [required] | - | No |
| `--no-share-activity-with-ai` | Set share_activity_with_ai to false | - | No |
| `--source-domain <value>` | Body field source_domain | - | No |
| `--source-url <value>` | Body field source_url | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics visitors run-activity-report`

Run activity report

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics settings`

Settings commands

**Subcommands:**

- `get-filtered-traffic` - How many events were counted and how many were kept out of the project's analytics, by reason, over the..
- `get` - Get a project's analytics traffic settings and whether datacenter detection is available on this instance
- `update` - Change a project's analytics traffic settings
- `list-traffic-filters` - List a project's traffic filters, newest first
- `create-traffic-filter` - Add a traffic filter
- `delete-traffic-filter` - Remove a traffic filter

#### `analytics settings get-filtered-traffic`

How many events were counted and how many were kept out of the project's analytics, by reason, over the..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--days <n>` | Window in days, ending now (default 7, max 30) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics settings get`

Get a project's analytics traffic settings and whether datacenter detection is available on this instance

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics settings update`

Change a project's analytics traffic settings

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--exclude-datacenter-traffic` | Body field exclude_datacenter_traffic | - | No |
| `--no-exclude-datacenter-traffic` | Set exclude_datacenter_traffic to false | - | No |
| `--exclude-spam-referrers` | Body field exclude_spam_referrers | - | No |
| `--no-exclude-spam-referrers` | Set exclude_spam_referrers to false | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics settings list-traffic-filters`

List a project's traffic filters, newest first

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--kind <value>` | Only filters of this kind (one of: referrer, ip, asn, country) | - | No |
| `--page <n>` | Page number, starting at 1 (default 1) | - | No |
| `--page-size <n>` | Page size (default 20, max 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics settings create-traffic-filter`

Add a traffic filter

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--kind <value>` | What a traffic filter matches on (one of: referrer, ip, asn, country) [required] | - | No |
| `--note <value>` | Why the filter exists, for whoever reviews it later | - | No |
| `--value <value>` | A domain or URL ('referrer'), an IP address or CIDR range ('ip'), a network number with or without the... [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `analytics settings delete-traffic-filter`

Remove a traffic filter

**Usage:** `analytics settings delete-traffic-filter <filter-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-api-callers`

Return the top callers (by client IP, ranked by request count) in a project's API traffic window

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Window start (ISO 8601) [required] | - | No |
| `--end-date <value>` | Window end (ISO 8601) [required] | - | No |
| `--limit <n>` | Max callers to return (default: 20, max: 100) | - | No |
| `--offset <n>` | Ranked callers to skip (default: 0) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics get-api-routes`

Return the top routes (by request count) in a project's API traffic, grouped by raw '(method, path)'

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Environment ID (optional) | - | No |
| `--start-date <value>` | Window start (ISO 8601) [required] | - | No |
| `--end-date <value>` | Window end (ISO 8601) [required] | - | No |
| `--limit <n>` | Max routes to return (default: 20, max: 100) | - | No |
| `--offset <n>` | Ranked routes to skip (default: 0) | - | No |
| `--json` | Output in JSON format | - | No |
