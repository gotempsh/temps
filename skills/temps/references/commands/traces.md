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
