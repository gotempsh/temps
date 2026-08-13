<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `analytics` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `analytics` (alias: `stats`)

View project analytics

**Subcommands:**

- `overview` (`o`) - Show analytics dashboard overview
- `top` - Show breakdown by dimension: pages, referrers, browsers, os, devices, countries, regions, cities, channels, events, languages, utm_source, utm_medium, utm_campaign
- `funnels` - Show funnel conversion metrics for all funnels
- `ai-agents` - Show AI crawler / provider breakdown (web /analytics/ai-agents)
- `ai-pages` - Show pages crawled by AI agents, with distinct-agent counts
- `ai-page` - Show which agents/providers crawled a single page (e.g. /docs)

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
