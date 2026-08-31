<!-- Generated from skills/temps-cli/references/COMMANDS.md. Do not edit manually. -->

# `services` command reference

Apply [the CLI runtime and safety contract](../cli-runtime.md) before executing a command. Runtime `--help` is authoritative.

## Contents

- [`services list`](#services-list)
- [`services create`](#services-create)
- [`services show`](#services-show)
- [`services remove`](#services-remove)
- [`services start`](#services-start)
- [`services stop`](#services-stop)
- [`services types`](#services-types)
- [`services projects`](#services-projects)
- [`services update`](#services-update)
- [`services upgrade`](#services-upgrade)
- [`services import`](#services-import)
- [`services link`](#services-link)
- [`services unlink`](#services-unlink)
- [`services connect`](#services-connect)
- [`services env`](#services-env)
- [`services env-var`](#services-env-var)
- [`services logs`](#services-logs)
- [`services slow-queries`](#services-slow-queries)
- [`services enable-pg-stat-statements`](#services-enable-pg-stat-statements)
- [`services metrics`](#services-metrics)
- [`services restore-capabilities`](#services-restore-capabilities)
- [`services list-backups`](#services-list-backups)
- [`services restore`](#services-restore)
- [`services restore-runs`](#services-restore-runs)
- [`services restore-run`](#services-restore-run)

## `services` (alias: `svc`)

Manage external services (databases, caches, storage)

**Subcommands:**

- `list` (`ls`) - List all external services
- `create` (`add`) - Create a new external service
- `show` - Show service details
- `remove` (`rm`) - Remove a service
- `start` - Start a stopped service
- `stop` - Stop a running service
- `types` - List available service types
- `projects` - List projects linked to a service
- `update` - Update a service
- `upgrade` - Upgrade a service to a newer version
- `import` - Import an existing external service
- `link` - Link a service to a project
- `unlink` - Unlink a service from a project
- `connect` - Get connection info for a service by name or slug
- `env` - Show environment variables for a linked service
- `env-var` - Get a specific environment variable
- `logs` - View persisted logs for an external service
- `slow-queries` - Show slowest PostgreSQL queries from pg_stat_statements
- `enable-pg-stat-statements` - Enable pg_stat_statements on a standalone Postgres service by restarting its container (drops active connections briefly)
- `metrics` - Resource and engine metrics for a database/cache/storage service
- `restore-capabilities` - Show what restore modes a service supports (in-place / new service / PITR)
- `list-backups` - List backups stored on an S3 source
- `restore` - Restore a service from a backup (in-place, new service, or PITR)
- `restore-runs` - List recent restore runs for a service
- `restore-run` - Show a single restore run

### `services list` (alias: `ls`)

List all external services

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

### `services create` (alias: `add`)

Create a new external service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --type <type>` | Service type (postgres, mongodb, redis, s3) | - | No |
| `-n, --name <name>` | Service name | - | No |
| `-s, --set <key=value>` | Set a parameter (repeatable) | `` | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `services show`

Show service details

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services remove` (alias: `rm`)

Remove a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `services start`

Start a stopped service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |

### `services stop`

Stop a running service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |

### `services types`

List available service types

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output in JSON format | - | No |

**Subcommands:**

- `info` - Show parameters schema for a service type (useful for automation)

#### `services types info`

Show parameters schema for a service type (useful for automation)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--json` | Output as raw JSON schema (default) | - | No |

### `services projects`

List projects linked to a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services update`

Update a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-n, --name <name>` | Docker image name (e.g., postgres:18-alpine) | - | No |
| `-s, --set <key=value>` | Set a parameter (repeatable) | `` | No |

### `services upgrade`

Upgrade a service to a newer version

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-v, --version <version>` | Docker image to upgrade to (e.g., postgres:18-alpine) | - | No |

### `services import`

Import an existing external service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-t, --type <type>` | Service type (postgres, mongodb, redis, s3) | - | No |
| `-n, --name <name>` | Service name | - | No |
| `--container-id <id>` | Container ID or name to import | - | No |
| `-s, --set <key=value>` | Set a parameter (repeatable) | `` | No |
| `--version <version>` | Optional version override | - | No |
| `-y, --yes` | Skip confirmation prompts (for automation) | - | No |

### `services link`

Link a service to a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |

### `services unlink`

Unlink a service from a project

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `-f, --force` | Skip confirmation | - | No |
| `-y, --yes` | Skip confirmation prompts (alias for --force) | - | No |

### `services connect`

Get connection info for a service by name or slug

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `--json` | Output in JSON format | - | No |

### `services env`

Show environment variables for a linked service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `--json` | Output in JSON format | - | No |

### `services env-var`

Get a specific environment variable

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-p, --project <slug>` | Project slug (auto-detected from .temps/config.json) | - | No |
| `--var <name>` | Environment variable name | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services logs`

View persisted logs for an external service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--from <datetime>` | Start of time range. ISO 8601 timestamp or a relative duration like "1h", "24h", "7d" (default: 24h ago) | - | No |
| `--to <datetime>` | End of time range. ISO 8601 timestamp (default: now) | - | No |
| `-l, --level <levels>` | Comma-separated log levels to include: ERROR,WARN,INFO,DEBUG,TRACE | - | No |
| `-n, --tail <lines>` | Maximum number of log lines to fetch (default: 200, max: 1000) | `200` | No |
| `-t, --text <query>` | Filter log lines by text (case-insensitive) | - | No |
| `--json` | Output raw JSON instead of formatted lines | - | No |

### `services slow-queries`

Show slowest PostgreSQL queries from pg_stat_statements

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--page <n>` | Page number (1-based, default: 1) | `1` | No |
| `--page-size <n>` | Rows per page (1–100, default: 20) | `20` | No |
| `--sort-by <column>` | Sort column: calls, total_exec_time_ms, mean_exec_time_ms, rows, cache_hit_ratio (default: mean_exec_time_ms) | - | No |
| `--sort-order <order>` | Sort direction: asc or desc (default: desc) | - | No |
| `--json` | Output raw JSON instead of a formatted table | - | No |

### `services enable-pg-stat-statements`

Enable pg_stat_statements on a standalone Postgres service by restarting its container (drops active connections briefly)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-y, --yes` | Skip the restart confirmation prompt (for automation) | - | No |

### `services metrics`

Resource and engine metrics for a database/cache/storage service

**Subcommands:**

- `latest` - Show the most recent value of every tracked metric
- `range` - Show a time-series range for a single metric
- `status` - Show when metrics were last received for a service
- `by-database` - Per-database metric breakdown (PostgreSQL services only)
- `enable` - Enable metric collection for a service (seeds default alert rules)
- `disable` - Disable metric collection for a service
- `alert-rules` - Manage monitoring alert rules for a service

#### `services metrics latest`

Show the most recent value of every tracked metric

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `services metrics range`

Show a time-series range for a single metric

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-m, --metric <name>` | Metric name, e.g. "pg.connections_active" | - | Yes |
| `-r, --range <window>` | Time window: 1h, 6h, 24h, 7d (default: 24h) | - | No |
| `-p, --percentile <n>` | Histogram percentile (0-100) instead of a plain average | - | No |
| `--json` | Output raw JSON instead of a formatted table | - | No |

#### `services metrics status`

Show when metrics were last received for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `services metrics by-database`

Per-database metric breakdown (PostgreSQL services only)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

#### `services metrics enable`

Enable metric collection for a service (seeds default alert rules)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |

#### `services metrics disable`

Disable metric collection for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |

#### `services metrics alert-rules`

Manage monitoring alert rules for a service

**Subcommands:**

- `list` (`ls`) - List alert rules for a service
- `create` (`add`) - Create an alert rule for a service
- `update` - Update an existing alert rule
- `remove` (`rm`) - Delete an alert rule

##### `services metrics alert-rules list` (alias: `ls`)

List alert rules for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

##### `services metrics alert-rules create` (alias: `add`)

Create an alert rule for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `-n, --name <name>` | Alert rule name | - | Yes |
| `-m, --metric <name>` | Metric name, e.g. "pg.connections_active" | - | Yes |
| `-c, --comparator <op>` | Comparator: >, <, >=, <= | - | Yes |
| `-t, --threshold <n>` | Threshold value that triggers the alert | - | Yes |
| `-s, --severity <level>` | warning or critical (default: warning) | - | No |
| `--for-duration <secs>` | Seconds the breach must persist before firing (default: 0) | - | No |
| `--disabled` | Create the rule disabled | - | No |
| `--json` | Output in JSON format | - | No |

##### `services metrics alert-rules update`

Update an existing alert rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--rule-id <id>` | Alert rule ID | - | Yes |
| `-n, --name <name>` | Alert rule name | - | No |
| `-m, --metric <name>` | Metric name | - | No |
| `-c, --comparator <op>` | Comparator: >, <, >=, <= | - | No |
| `-t, --threshold <n>` | Threshold value | - | No |
| `-s, --severity <level>` | warning or critical | - | No |
| `--for-duration <secs>` | Seconds the breach must persist before firing | - | No |
| `--enable` | Enable the rule | - | No |
| `--disable` | Disable the rule | - | No |
| `--json` | Output in JSON format | - | No |

##### `services metrics alert-rules remove` (alias: `rm`)

Delete an alert rule

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--rule-id <id>` | Alert rule ID | - | Yes |
| `-y, --yes` | Skip confirmation prompt | - | No |

### `services restore-capabilities`

Show what restore modes a service supports (in-place / new service / PITR)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services list-backups`

List backups stored on an S3 source

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--s3-source-id <id>` | S3 source ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services restore`

Restore a service from a backup (in-place, new service, or PITR)

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Source service ID (the service the backup came from) | - | Yes |
| `--backup-id <id>` | Backup ID to restore from (see `list-backups`) | - | Yes |
| `--new-service [name]` | Clone into a new service. Omit the value or pass "auto" to accept the auto-suggested name. | - | No |
| `--pitr <iso>` | Point-in-time recovery target, ISO 8601 timestamp (requires WAL-G backup). Combine with --new-service to route PITR into a new service. | - | No |
| `-y, --yes` | Skip confirmation | - | No |
| `--no-wait` | Return immediately without polling run status | - | No |
| `--json` | Output in JSON format | - | No |

### `services restore-runs`

List recent restore runs for a service

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Service ID | - | Yes |
| `--json` | Output in JSON format | - | No |

### `services restore-run`

Show a single restore run

**Options:**

| Flag | Description | Default | Required |
|------|-------------|---------|----------|
| `--id <id>` | Restore run ID | - | Yes |
| `--json` | Output in JSON format | - | No |
