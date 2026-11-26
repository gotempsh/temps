<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `proxy-logs` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `proxy-logs` (alias: `plogs`)

View proxy request logs and statistics

**Subcommands:**

- `list` (`ls`) - List proxy logs
- `show` - Show proxy log details
- `by-request` - Get proxy log by request ID
- `stats` - Get time bucket statistics (last 24 hours)
- `today` - Get today's request statistics
- `get-api-traffic-access` - Report whether the current principal may drill into a project's proxy logs
- `list-known-ai-agents` - List every AI agent the detector knows how to classify
- `get-ai-agent-pages` - Get the top pages accessed by a specific AI agent over a time window
- `get-ai-agent-timeline` - Time-bucketed AI-agent request volume, split by provider or agent
- `get-ai-status-breakdown` - HTTP status-class breakdown for AI-agent traffic — are bots being served (2xx) or hitting broken/blocked..
- `get-health` - Get user-traffic health summaries for multiple projects (last 1 hour)

### `proxy-logs list` (alias: `ls`)

List proxy logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--limit <n>` | Items per page (default: 20, max: 100) | - | No |
| `--page <n>` | Page number | - | No |
| `--project-id <id>` | Filter by project ID | - | No |
| `--environment-id <id>` | Filter by environment ID | - | No |
| `--method <method>` | Filter by HTTP method (GET, POST, etc.) | - | No |
| `--status-code <code>` | Filter by HTTP status code | - | No |
| `--host <host>` | Filter by host | - | No |
| `--path <path>` | Filter by path (partial match) | - | No |
| `--start-date <date>` | Start date (ISO 8601) | - | No |
| `--end-date <date>` | End date (ISO 8601) | - | No |
| `--sort-by <field>` | Sort by field (default: timestamp) | - | No |
| `--sort-order <order>` | Sort order: asc or desc (default: desc) | - | No |
| `--is-bot` | Filter for bot requests only | - | No |
| `--has-error` | Filter for requests with errors only | - | No |
| `--path-exact <value>` | Only this exact path (--path matches a prefix) | - | No |
| `--session-id <n>` | Only requests of this analytics session | - | No |
| `--visitor-id <n>` | Only requests of this visitor | - | No |
| `--exclude-bots` | Leave out requests from bots | - | No |
| `--no-exclude-bots` | Set exclude_bots to false | - | No |
| `--exclude-synthetic` | Leave out synthetic traffic (uptime checks, Temps itself) | - | No |
| `--no-exclude-synthetic` | Set exclude_synthetic to false | - | No |
| `--is-ai-agent` | Only (or, with --no-is-ai-agent, never) AI agent requests | - | No |
| `--no-is-ai-agent` | Set is_ai_agent to false | - | No |
| `--ai-agent <value>` | Only this AI agent (e.g. GPTBot) | - | No |
| `--ai-provider <value>` | Only AI agents of this provider (e.g. OpenAI) | - | No |
| `--request-size-min <n>` | Smallest request body, in bytes | - | No |
| `--request-size-max <n>` | Largest request body, in bytes | - | No |
| `--response-size-min <n>` | Smallest response body, in bytes | - | No |
| `--response-size-max <n>` | Largest response body, in bytes | - | No |
| `--response-time-min <n>` | Fastest response, in milliseconds | - | No |
| `--response-time-max <n>` | Slowest response, in milliseconds | - | No |

### `proxy-logs show`

Show proxy log details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Proxy log ID | - | Yes |
| `--project-id <id>` | Authorize the lookup within this project | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs by-request`

Get proxy log by request ID

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--request-id <id>` | Request ID | - | No |
| `--project-id <id>` | Authorize the lookup within this project | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs stats`

Get time bucket statistics (last 24 hours)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--has-project` | Only requests routed to a project (or, with --no-has-project, to none) | - | No |
| `--no-has-project` | Set has_project to false | - | No |
| `--status-code-class <value>` | Only this status class (one of: 2xx, 3xx, 4xx, 5xx) | - | No |

### `proxy-logs today`

Get today's request statistics

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |
| `--status-code-class <value>` | Only this status class (one of: 2xx, 3xx, 4xx, 5xx) | - | No |

### `proxy-logs get-api-traffic-access`

Report whether the current principal may drill into a project's proxy logs

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs list-known-ai-agents`

List every AI agent the detector knows how to classify

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `proxy-logs get-ai-agent-pages`

Get the top pages accessed by a specific AI agent over a time window

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--agent <value>` | Canonical agent name to filter by (e.g. 'ChatGPT-User', 'ClaudeBot') [required] | - | No |
| `--project-id <n>` | Filter by project ID | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--start-time <value>` | Start time (ISO 8601) | - | No |
| `--end-time <value>` | End time (ISO 8601) | - | No |
| `--limit <n>` | Maximum rows to return | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs get-ai-agent-timeline`

Time-bucketed AI-agent request volume, split by provider or agent

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Filter by project ID | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--start-time <value>` | Start time (ISO 8601) | - | No |
| `--end-time <value>` | End time (ISO 8601) | - | No |
| `--group-by <value>` | Grouping dimension: 'provider' (default) or 'agent' | - | No |
| `--bucket <value>` | Bucket interval override (e.g. '1 hour', '1 day') | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs get-ai-status-breakdown`

HTTP status-class breakdown for AI-agent traffic — are bots being served (2xx) or hitting broken/blocked..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Filter by project ID (recommended for per-project analytics) | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--start-time <value>` | Start time (ISO 8601) | - | No |
| `--end-time <value>` | End time (ISO 8601) | - | No |
| `--limit <n>` | Maximum rows to return | - | No |
| `--path <value>` | Optional exact path filter | - | No |
| `--json` | Output in JSON format | - | No |

### `proxy-logs get-health`

Get user-traffic health summaries for multiple projects (last 1 hour)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-ids <value>` | Comma-separated list of project IDs [required] | - | No |
| `--start-time <value>` | Optional start time (ISO 8601) | - | No |
| `--end-time <value>` | Optional end time (ISO 8601) | - | No |
| `--is-bot` | Filter by bot detection | - | No |
| `--no-is-bot` | Set is_bot to false | - | No |
| `--json` | Output in JSON format | - | No |
