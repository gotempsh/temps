<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `otel` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`otel ingest-errors`](#otel-ingest-errors)
- [`otel pipeline-history`](#otel-pipeline-history)
- [`otel alerts`](#otel-alerts)
- [`otel get-cloud-backfill-status`](#otel-get-cloud-backfill-status)
- [`otel dashboards`](#otel-dashboards)
- [`otel genai`](#otel-genai)
- [`otel has-traces`](#otel-has-traces)
- [`otel get-health`](#otel-get-health)
- [`otel insights`](#otel-insights)
- [`otel logs`](#otel-logs)
- [`otel get-pipeline-stats`](#otel-get-pipeline-stats)
- [`otel get-quota`](#otel-get-quota)
- [`otel observe`](#otel-observe)

## `otel`

Inspect the OTLP ingest pipeline itself — throughput, drops and failure reasons (server-wide, not project-scoped; see "temps metrics" to query ingested application metrics)

**Subcommands:**

- `ingest-errors` - Show why ingest batches were dropped, grouped by signal and failure reason
- `pipeline-history` - Show pipeline counter trends over time (received/stored/dropped per signal)
- `alerts` - Alerts commands
- `get-cloud-backfill-status` - Status of the most recent Temps Cloud telemetry backfill for a project
- `dashboards` - Dashboards commands
- `genai` - Genai commands
- `has-traces` - Whether a project has ever received at least one trace span
- `get-health` - Get health summaries for a project
- `insights` - Insights commands
- `logs` - Logs commands
- `get-pipeline-stats` - Get OTel pipeline statistics (admin/system view)
- `get-quota` - Get storage quota for a project
- `observe` - Observe commands

### `otel ingest-errors`

Show why ingest batches were dropped, grouped by signal and failure reason

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--limit <n>` | Max failure groups to return (default: 20, server cap: 100) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel pipeline-history`

Show pipeline counter trends over time (received/stored/dropped per signal)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--period <period>` | Time period: 1h, 6h, 24h, 7d (server presets), or today/<n>h/<n>d resolved locally | `24h` | No |
| `--start-time <iso>` | Explicit window start (RFC 3339) — overrides --period | - | No |
| `--end-time <iso>` | Explicit window end (RFC 3339) — overrides --period | - | No |
| `--json` | Output in JSON format | - | No |

### `otel alerts`

Alerts commands

**Subcommands:**

- `list` - List alert rules for a project (newest first, paginated)
- `create` - Create a new alert rule for a project
- `preview` - Backtest an anomaly detector over a time range without saving a rule
- `delete` - Delete an alert rule
- `get` - Fetch a single alert rule by id
- `update` - Update an alert rule's fields

#### `otel alerts list`

List alert rules for a project (newest first, paginated)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Page size (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel alerts create`

Create a new alert rule for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--aggregation <value>` | One of 'avg\|sum\|min\|max\|count\|rate\|p50\|p90\|p95\|p99' [required] | - | No |
| `--detection-config <json>` | The detector: a discriminated union keyed by 'kind' (JSON) [required] | - | No |
| `--dynamic-alerts` | When true (and 'group_by' is set) fire one independent alarm per breaching series | - | No |
| `--no-dynamic-alerts` | Set dynamic_alerts to false | - | No |
| `--enabled` | Body field enabled [required] | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--for-duration-secs <n>` | Body field for_duration_secs [required] | - | No |
| `--group-by <value>` | Label keys to break the metric down by, e.g. '["endpoint","region"]' (repeatable) | - | No |
| `--grouped-notification-threshold <n>` | When more than this many series transition to firing in the same tick, only the first gets the expensive... | - | No |
| `--label-filters <json>` | AND-combined label equality filters: '[["key","value"],…]' (JSON) | - | No |
| `--max-series <n>` | Cardinality cap for dynamic alerting: at most this many series (top by '\|value\|') | - | No |
| `--metric-name <value>` | The OTel metric to watch | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--project-id <n>` | Body field project_id [required] | - | No |
| `--severity <value>` | One of 'info\|warning\|critical' [required] | - | No |
| `--source <json>` | What the rule watches: the metric above (the default), or the number of log lines matching a saved search... (JSON) | - | No |
| `--window-secs <n>` | Body field window_secs [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel alerts preview`

Backtest an anomaly detector over a time range without saving a rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--aggregation <value>` | One of 'avg\|sum\|min\|max\|count\|rate\|p50\|p90\|p95\|p99' [required] | - | No |
| `--detection-config <json>` | The detector to backtest. 'static' and 'anomaly' are supported — the kinds the evaluator actually runs (JSON) [required] | - | No |
| `--end-time <value>` | RFC 3339; defaults to now | - | No |
| `--metric-name <value>` | Body field metric_name [required] | - | No |
| `--project-id <n>` | Body field project_id [required] | - | No |
| `--start-time <value>` | RFC 3339; defaults to 7 days before 'end_time' | - | No |
| `--window-secs <n>` | Body field window_secs [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel alerts delete`

Delete an alert rule

**Usage:** `otel alerts delete <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Owning project ID (scopes the delete) [required] | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel alerts get`

Fetch a single alert rule by id

**Usage:** `otel alerts get <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Owning project ID (scopes the lookup) [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel alerts update`

Update an alert rule's fields

**Usage:** `otel alerts update <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Owning project ID (scopes the update) [required] | - | No |
| `--aggregation <value>` | Body field aggregation | - | No |
| `--detection-config <json>` | The typed detector definition stored (as jsonb) in 'metric_alert_rules.detection_config' (JSON) | - | No |
| `--dynamic-alerts` | Toggles per-series ("dynamic") alerting (absent = leave unchanged) | - | No |
| `--no-dynamic-alerts` | Set dynamic_alerts to false | - | No |
| `--enabled` | Body field enabled | - | No |
| `--no-enabled` | Set enabled to false | - | No |
| `--for-duration-secs <n>` | Body field for_duration_secs | - | No |
| `--group-by <value>` | Replaces the group_by keys wholesale when present (absent = leave unchanged) (repeatable) | - | No |
| `--grouped-notification-threshold <n>` | Updates the notification-grouping threshold (absent = leave unchanged) | - | No |
| `--label-filters <json>` | Replaces the label filters wholesale when present (absent = leave unchanged) (JSON) | - | No |
| `--max-series <n>` | Updates the dynamic-alerting cardinality cap (absent = leave unchanged) | - | No |
| `--metric-name <value>` | Body field metric_name | - | No |
| `--name <value>` | Body field name | - | No |
| `--severity <value>` | Body field severity | - | No |
| `--source <json>` | What series a rule is evaluated on (ADR 108 D1) (JSON) | - | No |
| `--window-secs <n>` | Body field window_secs | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel get-cloud-backfill-status`

Status of the most recent Temps Cloud telemetry backfill for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel dashboards`

Dashboards commands

**Subcommands:**

- `list` - List dashboards for a project (newest first, paginated)
- `create` - Create a new dashboard for a project
- `delete` - Delete a dashboard
- `get` - Fetch a single dashboard by id
- `update` - Update a dashboard's name and/or layout

#### `otel dashboards list`

List dashboards for a project (newest first, paginated)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--page <n>` | Page number (default: 1) | - | No |
| `--page-size <n>` | Page size (default: 20, max: 100) | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel dashboards create`

Create a new dashboard for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--layout <json>` | The typed layout persisted (as JSONB) in 'metric_dashboards.layout' (JSON) [required] | - | No |
| `--name <value>` | Body field name [required] | - | No |
| `--project-id <n>` | Body field project_id [required] | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel dashboards delete`

Delete a dashboard

**Usage:** `otel dashboards delete <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Owning project ID (scopes the delete) [required] | - | No |
| `-y, --yes` | Do not ask for confirmation | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel dashboards get`

Fetch a single dashboard by id

**Usage:** `otel dashboards get <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Owning project ID (scopes the lookup) [required] | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel dashboards update`

Update a dashboard's name and/or layout

**Usage:** `otel dashboards update <id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Owning project ID (scopes the update) [required] | - | No |
| `--layout <json>` | The typed layout persisted (as JSONB) in 'metric_dashboards.layout' (JSON) | - | No |
| `--name <value>` | Body field name | - | No |
| `--from-file <path>` | Start the request body from a JSON file (- for stdin); flags override it | - | No |
| `--set <field=value>` | Set a body field by path, e.g. limits.memory=512 (repeatable) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel genai`

Genai commands

**Subcommands:**

- `query-traces` - Query GenAI trace summaries — traces containing spans with 'gen_ai.*' attributes
- `get-trace` - Get GenAI span details for a specific trace

#### `otel genai query-traces`

Query GenAI trace summaries — traces containing spans with 'gen_ai.*' attributes

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--service-name <value>` | Filter by service name | - | No |
| `--gen-ai-system <value>` | Filter by AI system (openai, anthropic, etc.) | - | No |
| `--gen-ai-model <value>` | Filter by model (gpt-6.2-sol, claude-sonnet-5-5, etc.) | - | No |
| `--start-time <value>` | Start time (RFC 3339) | - | No |
| `--end-time <value>` | End time (RFC 3339) | - | No |
| `--limit <n>` | Max traces to return (default: 50, max: 100) | - | No |
| `--offset <n>` | Offset for pagination | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel genai get-trace`

Get GenAI span details for a specific trace

**Usage:** `otel genai get-trace <trace-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--start-time <value>` | Window start (RFC 3339); pair with end_time, max 31 days | - | No |
| `--end-time <value>` | Window end (RFC 3339); pair with start_time, max 31 days | - | No |
| `--json` | Output in JSON format | - | No |

### `otel has-traces`

Whether a project has ever received at least one trace span

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel get-health`

Get health summaries for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--json` | Output in JSON format | - | No |

### `otel insights`

Insights commands

**Subcommands:**

- `list` - List anomaly insights for a project

#### `otel insights list`

List anomaly insights for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--status <value>` | Filter by status (active, resolved) | - | No |
| `--limit <n>` | Max insights to return (default: 20, max: 100) | - | No |
| `--offset <n>` | Offset for pagination | - | No |
| `--json` | Output in JSON format | - | No |

### `otel logs`

Logs commands

**Subcommands:**

- `query` - Query log records with optional filters

#### `otel logs query`

Query log records with optional filters

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--severity <value>` | Filter by severity (TRACE, DEBUG, INFO, WARN, ERROR, FATAL) | - | No |
| `--service-name <value>` | Filter by service name | - | No |
| `--search <value>` | Full-text search in log body (ILIKE) | - | No |
| `--attribute-key <value>` | Exact attribute key; pair with attribute_value | - | No |
| `--attribute-value <value>` | Exact string or numeric text, preserving int64 precision | - | No |
| `--attribute-type <value>` | Optional native declared type; older records without type metadata do not match (one of: string, boolean, integer, double, array, object, bytes) | - | No |
| `--attribute-op <value>` | Default eq; numeric types also support lt/lte/gt/gte (one of: eq, ne, lt, lte, gt, gte) | - | No |
| `--trace-id <value>` | Filter by correlated trace ID | - | No |
| `--workload-kind <value>` | Only what the platform recorded about one workload of the project (sandbox, deployment, ...); pair with workload_id | - | No |
| `--workload-id <n>` | The workload's numeric id; pair with workload_kind | - | No |
| `--start-time <value>` | Start time (RFC 3339) | - | No |
| `--end-time <value>` | End time (RFC 3339) | - | No |
| `--limit <n>` | Max logs to return (default: 100, max: 1000) | - | No |
| `--offset <n>` | Offset for pagination | - | No |
| `--json` | Output in JSON format | - | No |

### `otel get-pipeline-stats`

Get OTel pipeline statistics (admin/system view)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `otel get-quota`

Get storage quota for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--json` | Output in JSON format | - | No |

### `otel observe`

Observe commands

**Subcommands:**

- `observability-list-events` - List a merged page of observability events for a project
- `observability-full-event` - Fetch the un-truncated form of one event by '(kind, id)'

#### `otel observe observability-list-events`

List a merged page of observability events for a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--kinds <value>` | Comma-separated kinds: 'request,span,error,revenue' | - | No |
| `--from <value>` | Inclusive lower bound on event timestamp (ISO 8601, 'Z' suffix) | - | No |
| `--to <value>` | Inclusive upper bound on event timestamp | - | No |
| `--deployment-id <n>` | Query parameter deployment_id | - | No |
| `--environment-id <n>` | Query parameter environment_id | - | No |
| `--search <value>` | Free-text substring matched against per-kind summary fields (request path / error class / revenue event_type) | - | No |
| `--limit <n>` | Page size (default 50, max 200) | - | No |
| `--hide-bots` | When 'true', exclude bot/crawler request rows | - | No |
| `--no-hide-bots` | Set hide_bots to false | - | No |
| `--json` | Output in JSON format | - | No |

#### `otel observe observability-full-event`

Fetch the un-truncated form of one event by '(kind, id)'

**Usage:** `otel observe observability-full-event <kind> <event-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug-or-id>` | Project slug or ID (default: the linked project) | - | No |
| `--ts <value>` | The row's event timestamp as returned by the list endpoint | - | No |
| `--json` | Output in JSON format | - | No |
