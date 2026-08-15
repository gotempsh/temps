<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `analytics` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `analytics` (alias: `stats`)

View project analytics

**Subcommands:**

- `overview` (`o`) - Show analytics dashboard overview
- `top` - Show breakdown by dimension: pages, referrers, browsers, os, devices, countries, regions, cities, channels, events, languages, utm_source, utm_medium, utm_campaign
- `funnels` - Show funnel conversion metrics for all funnels
- `performance` (`speed`) - Show real-user Web Vitals and optional dimension breakdowns
- `ai-agents` - Show AI crawler / provider breakdown (web /analytics/ai-agents)
- `ai-pages` - Show pages crawled by AI agents, with distinct-agent counts
- `ai-page` - Show which agents/providers crawled a single page (e.g. /docs)
- `api-overview` - Show API traffic timeseries (requests, errors, latency) from /api-analytics/timeseries
- `api-routes` - Show top API routes by request count from /api-analytics/routes
- `api-callers` - Show top API callers by client IP from /api-analytics/callers
- `api-summary` - Show an AI-generated summary of API traffic from /api-analytics/summary (requires AI Assistance to be configured and enabled on the project)

### `analytics overview` (alias: `o`)

Show analytics dashboard overview

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--json` | Output in JSON format | - | No |

### `analytics top`

Show breakdown by dimension: pages, referrers, browsers, os, devices, countries, regions, cities, channels, events, languages, utm_source, utm_medium, utm_campaign

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--limit <n>` | Number of results (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `analytics funnels`

Show funnel conversion metrics for all funnels

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `7d` | No |
| `--json` | Output in JSON format | - | No |

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

### `analytics ai-page`

Show which agents/providers crawled a single page (e.g. /docs)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 24h, 7d, 30d) | `24h` | No |
| `--limit <n>` | Number of rows to fetch (default: 50, max: 100) | - | No |
| `--group-by <mode>` | Group rows by "agent" (default) or "provider" | `agent` | No |
| `--json` | Output in JSON format | - | No |

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
| `--json` | Output in JSON format | - | No |

### `analytics api-summary`

Show an AI-generated summary of API traffic from /api-analytics/summary (requires AI Assistance to be configured and enabled on the project)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <project>` | Project slug or ID | - | No |
| `--environment-id <id>` | Restrict traffic to one environment ID | - | No |
| `--period <period>` | Time period: today, <n>h, <n>d, <n>m (e.g. 1h, 6h, 48h, 7d, 30d, 3m) | `24h` | No |
| `--json` | Output in JSON format | - | No |
