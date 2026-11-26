<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `traces` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## `traces` (alias: `trace`)

Inspect distributed traces and operation latency

**Subcommands:**

- `span-stats` (`operations`, `ops`) - Rank operations by time spent, latency percentiles, or inconsistency
- `trend` - Show how one operation's latency moved, day by day, with deployments
- `regressions` - List the operations that got slower or faster after a deployment
- `breakdown` - Show where the time inside one operation goes: queries, downstream calls, its own code
- `page` - Compare a page's web vitals with the backend requests that served it
- `show` (`get`) - Show every span of one trace: timing, service, status, attributes and events
- `query-global` - Query global traces
- `query-global-summaries` - Query global trace summaries
- `get-unified` - Assemble a unified cross-project span waterfall (Phase 2)
- `query-summaries` - Query trace summaries — one row per trace with span count, error count, root span info, and proper..
- `query` - Query trace spans with optional filters
- `get-cross-siblings` - Discover sibling projects that share the same 'trace_id' (Phase 1 banner)

### `traces span-stats` (alias: `operations`, `ops`)

Rank operations by time spent, latency percentiles, or inconsistency

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | No |
| `--project-ids <ids>` | Comma-separated project IDs to rank across, e.g. 4,5,6 (max 50) | - | No |
| `--since <duration>` | Relative window: 30m, 24h, 7d (max 31d) | `24h` | No |
| `--start-time <iso>` | Window start (ISO 8601); overrides --since | - | No |
| `--end-time <iso>` | Window end (ISO 8601); defaults to now | - | No |
| `--service <name>` | Only this service | - | No |
| `--operation <name>` | Only this operation (exact span name) | - | No |
| `--search <text>` | Only operations whose name contains this text | - | No |
| `--kind <kind>` | Span kind (server, client, internal, producer, consumer, unspecified) | - | No |
| `--status <status>` | Span status (ok, error, unset) | - | No |
| `--environment-id <id>` | Only this environment | - | No |
| `--deployment-id <id>` | Only this deployment | - | No |
| `--attributes <pairs>` | Span attribute filters, e.g. db.system=postgresql | - | No |
| `--min-duration-ms <ms>` | Ignore spans faster than this | - | No |
| `--min-count <n>` | Drop operations with fewer samples than this | - | No |
| `--apdex-ms <ms>` | Apdex threshold: calls at or under this satisfy, up to 4x tolerate (default 500) | - | No |
| `--sort-by <field>` | Ranking (total_time, p50, p95, p99, max, avg, stddev, count, errors, error_rate, variability, tail_ratio) | `total_time` | No |
| `--sort-order <order>` | asc or desc | `desc` | No |
| `--limit <n>` | Rows to show (max 100) | `20` | No |
| `--offset <n>` | Page offset | - | No |
| `--json` | Output in JSON format | - | No |

### `traces trend`

Show how one operation's latency moved, day by day, with deployments

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | No |
| `--service <name>` | Service of the operation | - | No |
| `--operation <name>` | The operation (span name) | - | No |
| `--days <n>` | Days to show (max 30) | `14` | No |
| `--json` | Output the hourly points in JSON format | - | No |

### `traces regressions`

List the operations that got slower or faster after a deployment

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | No |
| `--deployment-id <id>` | Deployment to compare across | - | No |
| `--json` | Output in JSON format | - | No |

### `traces breakdown`

Show where the time inside one operation goes: queries, downstream calls, its own code

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | No |
| `--service <name>` | Service of the operation | - | No |
| `--operation <name>` | The operation (span name) | - | No |
| `--sample <kind>` | Calls to sample (recent, slow) | `recent` | No |
| `--attribute <key>` | Also split the calls by this attribute, e.g. http.route | - | No |
| `--json` | Output in JSON format | - | No |

### `traces page`

Compare a page's web vitals with the backend requests that served it

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project ID | - | No |
| `--path <path>` | The page path, e.g. /checkout | - | No |
| `--json` | Output in JSON format | - | No |

### `traces show` (alias: `get`)

Show every span of one trace: timing, service, status, attributes and events

**Usage:** `traces show <trace-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <id>` | Project the trace belongs to | - | Yes |
| `--start-time <iso>` | Look for the trace from this time (ISO 8601); pair with --end-time | - | No |
| `--end-time <iso>` | Look for the trace up to this time (ISO 8601); pair with --start-time | - | No |
| `--json` | Output the spans in JSON format | - | No |

### `traces query-global`

Query global traces

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Query parameter project_id | - | No |
| `--trace-id <value>` | Query parameter trace_id | - | No |
| `--service-name <value>` | Query parameter service_name | - | No |
| `--status <value>` | Query parameter status | - | No |
| `--min-duration-ms <number>` | Query parameter min_duration_ms | - | No |
| `--start-time <value>` | Query parameter start_time | - | No |
| `--end-time <value>` | Query parameter end_time | - | No |
| `--environment-id <n>` | Query parameter environment_id | - | No |
| `--deployment-id <n>` | Query parameter deployment_id | - | No |
| `--attributes <value>` | Filter by span attributes as comma-separated key=value pairs. e.g.... | - | No |
| `--name-pattern <value>` | Filter by span name pattern (ILIKE) | - | No |
| `--sort-by <value>` | Sort field for the trace-summaries list: "start_time" (default) or "duration" | - | No |
| `--sort-order <value>` | Sort direction: "asc" or "desc" (default) | - | No |
| `--limit <n>` | Query parameter limit | - | No |
| `--offset <n>` | Query parameter offset | - | No |
| `--json` | Output in JSON format | - | No |

### `traces query-global-summaries`

Query global trace summaries

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Query parameter project_id | - | No |
| `--trace-id <value>` | Query parameter trace_id | - | No |
| `--service-name <value>` | Query parameter service_name | - | No |
| `--status <value>` | Query parameter status | - | No |
| `--min-duration-ms <number>` | Query parameter min_duration_ms | - | No |
| `--start-time <value>` | Query parameter start_time | - | No |
| `--end-time <value>` | Query parameter end_time | - | No |
| `--environment-id <n>` | Query parameter environment_id | - | No |
| `--deployment-id <n>` | Query parameter deployment_id | - | No |
| `--attributes <value>` | Filter by span attributes as comma-separated key=value pairs. e.g.... | - | No |
| `--name-pattern <value>` | Filter by span name pattern (ILIKE) | - | No |
| `--sort-by <value>` | Sort field for the trace-summaries list: "start_time" (default) or "duration" | - | No |
| `--sort-order <value>` | Sort direction: "asc" or "desc" (default) | - | No |
| `--limit <n>` | Query parameter limit | - | No |
| `--offset <n>` | Query parameter offset | - | No |
| `--json` | Output in JSON format | - | No |

### `traces get-unified`

Assemble a unified cross-project span waterfall (Phase 2)

**Usage:** `traces get-unified <trace-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--start-time <value>` | Window start (RFC 3339); pair with end_time, max 31 days | - | No |
| `--end-time <value>` | Window end (RFC 3339); pair with start_time, max 31 days | - | No |
| `--json` | Output in JSON format | - | No |

### `traces query-summaries`

Query trace summaries — one row per trace with span count, error count, root span info, and proper..

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--trace-id <value>` | Filter by trace ID | - | No |
| `--service-name <value>` | Filter by service name | - | No |
| `--status <value>` | Filter by status (OK, ERROR) | - | No |
| `--min-duration-ms <number>` | Minimum trace duration in ms | - | No |
| `--start-time <value>` | Start time (RFC 3339) | - | No |
| `--end-time <value>` | End time (RFC 3339) | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--deployment-id <n>` | Filter by deployment ID | - | No |
| `--attributes <value>` | Filter by span attributes as comma-separated key=value pairs, e.g.... | - | No |
| `--name-pattern <value>` | Filter by span name pattern (ILIKE) | - | No |
| `--sort-by <value>` | Sort field: 'start_time' (default) or 'duration' | - | No |
| `--sort-order <value>` | Sort direction: 'asc' or 'desc' (default) | - | No |
| `--include-total` | Compute the 'total' count (default: true) | - | No |
| `--no-include-total` | Set include_total to false | - | No |
| `--limit <n>` | Max traces to return (default: 50, max: 100) | - | No |
| `--offset <n>` | Offset for pagination | - | No |
| `--json` | Output in JSON format | - | No |

### `traces query`

Query trace spans with optional filters

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--project-id <n>` | Project ID [required] | - | No |
| `--trace-id <value>` | Filter by trace ID | - | No |
| `--service-name <value>` | Filter by service name | - | No |
| `--status <value>` | Filter by status (OK, ERROR, UNSET) | - | No |
| `--min-duration-ms <number>` | Minimum span duration in ms | - | No |
| `--start-time <value>` | Start time (RFC 3339) | - | No |
| `--end-time <value>` | End time (RFC 3339) | - | No |
| `--environment-id <n>` | Filter by environment ID | - | No |
| `--deployment-id <n>` | Filter by deployment ID | - | No |
| `--attributes <value>` | Filter by span attributes as comma-separated key=value pairs, e.g.... | - | No |
| `--name-pattern <value>` | Filter by span name pattern (ILIKE) | - | No |
| `--limit <n>` | Max spans to return (default: 100, max: 1000) | - | No |
| `--offset <n>` | Offset for pagination | - | No |
| `--json` | Output in JSON format | - | No |

### `traces get-cross-siblings`

Discover sibling projects that share the same 'trace_id' (Phase 1 banner)

**Usage:** `traces get-cross-siblings <trace-id> [options]`

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--exclude-project-id <n>` | Project ID to exclude (the caller's own project) so the UI does not render a self-link | - | No |
| `--json` | Output in JSON format | - | No |
